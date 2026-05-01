//! Pocopine-owned wasm splitting analysis.
//!
//! The old prototype splitter treated relocation records as the source of
//! truth and raw-copied function bodies. This crate starts from the opposite
//! invariant: executable code is parsed instruction-by-instruction, and every
//! index-bearing operator is recorded before any future emitter is allowed to
//! split or rewrite a module.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use wasm_encoder::reencode::{Error as ReencodeError, Reencode};
use wasmparser::{ExternalKind, Operator, Payload, TypeRef};

pub type WasmResult<T> = Result<T, wasmparser::BinaryReaderError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dependency {
    Function(u32),
    Type(u32),
    Table(u32),
    Memory(u32),
    Global(u32),
    Tag(u32),
    Data(u32),
    Element(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexSpaces {
    pub functions: u32,
    pub types: u32,
    pub tables: u32,
    pub memories: u32,
    pub globals: u32,
    pub tags: u32,
    pub data: u32,
    pub elements: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexUse {
    pub offset: usize,
    pub dependency: Dependency,
    pub operator: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionAnalysis {
    pub index: u32,
    pub type_index: u32,
    pub defined: bool,
    pub body: Option<Vec<u8>>,
    pub index_uses: Vec<IndexUse>,
    pub dependencies: BTreeSet<Dependency>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModuleAnalysis {
    pub imported_functions: u32,
    pub index_spaces: IndexSpaces,
    pub imports: BTreeSet<Dependency>,
    pub function_imports: BTreeMap<u32, FunctionImport>,
    pub table_imports: BTreeMap<u32, TableImport>,
    pub memory_imports: BTreeMap<u32, MemoryImport>,
    pub global_imports: BTreeMap<u32, GlobalImport>,
    pub tag_imports: BTreeMap<u32, TagImport>,
    pub types: Vec<wasmparser::FuncType>,
    pub tables: Vec<TableAnalysis>,
    pub memories: Vec<MemoryAnalysis>,
    pub globals: Vec<GlobalAnalysis>,
    pub tags: Vec<TagAnalysis>,
    pub data_segments: Vec<DataAnalysis>,
    pub elements: Vec<ElementAnalysis>,
    pub functions: Vec<FunctionAnalysis>,
    pub exports: Vec<ExportAnalysis>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionImport {
    pub function: u32,
    pub module: String,
    pub name: String,
    pub type_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableImport {
    pub table: u32,
    pub module: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryImport {
    pub memory: u32,
    pub module: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalImport {
    pub global: u32,
    pub module: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagImport {
    pub tag: u32,
    pub module: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableAnalysis {
    pub index: u32,
    pub ty: wasmparser::TableType,
    pub imported: bool,
    pub has_init_expr: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAnalysis {
    pub index: u32,
    pub ty: wasmparser::MemoryType,
    pub imported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalAnalysis {
    pub index: u32,
    pub ty: wasmparser::GlobalType,
    pub imported: bool,
    pub init_expr: Option<ConstExprAnalysis>,
    pub dependencies: BTreeSet<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagAnalysis {
    pub index: u32,
    pub ty: wasmparser::TagType,
    pub imported: bool,
    pub dependencies: BTreeSet<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataAnalysis {
    pub index: u32,
    pub kind: DataKindAnalysis,
    pub data: Vec<u8>,
    pub dependencies: BTreeSet<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataKindAnalysis {
    Passive,
    Active {
        memory_index: u32,
        offset_expr: ConstExprAnalysis,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementAnalysis {
    pub index: u32,
    pub kind: ElementKindAnalysis,
    pub items: ElementItemsAnalysis,
    pub dependencies: BTreeSet<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementKindAnalysis {
    Passive,
    Active {
        table_index: Option<u32>,
        offset_expr: ConstExprAnalysis,
    },
    Declared,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstExprAnalysis {
    I32Const(i32),
    GlobalGet(u32),
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementItemsAnalysis {
    Functions(Vec<u32>),
    Expressions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSplitRoot {
    pub name: String,
    pub roots: Vec<Dependency>,
}

/// Ownership of a function inferred from its mangled name.
///
/// Used to break ties when a function is reached only via the shared funcref
/// table — direct call edges remain authoritative, but indirect-only reachability
/// would otherwise pull every table entry into every chunk's closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionOwner {
    /// Belongs to no route; lives in shell.
    Shell,
    /// Owned by a single route (route id matches).
    Route(String),
    /// Multiple route ids appear in the mangled name.
    Shared(Vec<String>),
    /// Function has no name; ownership cannot be inferred from naming.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClosureMode {
    Full,
    Direct,
}

impl FunctionOwner {
    /// Classify a function name against the project's route ids.
    ///
    /// A name is matched against `::routes::<id>::` substrings; multiple matches
    /// produce `Shared`. Names without any match are `Shell`. `None` produces
    /// `Unknown` (treated conservatively by the partitioner).
    pub fn from_name(name: Option<&str>, route_ids: &[String]) -> Self {
        let Some(name) = name else {
            return Self::Unknown;
        };
        let mut hits: Vec<String> = Vec::new();
        for route_id in route_ids {
            let needle = format!("::routes::{route_id}::");
            if name.contains(&needle) && !hits.contains(route_id) {
                hits.push(route_id.clone());
            }
        }
        match hits.len() {
            0 => Self::Shell,
            1 => Self::Route(hits.into_iter().next().unwrap()),
            _ => Self::Shared(hits),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportAnalysis {
    pub name: String,
    pub kind: ExternalKind,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    FunctionTypeIndexOutOfBounds {
        function: u32,
        type_index: u32,
        limit: u32,
    },
    FunctionIndexOutOfBounds {
        function: u32,
        offset: usize,
        operator: &'static str,
        dependency: Dependency,
        limit: u32,
    },
    ExportIndexOutOfBounds {
        export: String,
        kind: ExternalKind,
        index: u32,
        limit: u32,
    },
    ElementIndexOutOfBounds {
        element: u32,
        dependency: Dependency,
        limit: u32,
    },
    GlobalIndexOutOfBounds {
        global: u32,
        dependency: Dependency,
        limit: u32,
    },
    DataIndexOutOfBounds {
        data: u32,
        dependency: Dependency,
        limit: u32,
    },
    TagIndexOutOfBounds {
        tag: u32,
        dependency: Dependency,
        limit: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    InvalidModule(Vec<ValidationError>),
    RootIndexOutOfBounds { dependency: Dependency, limit: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    MissingFunctionTypeRemap {
        chunk: String,
        function: u32,
        type_index: u32,
    },
    MissingInstructionRemap {
        chunk: String,
        function: u32,
        offset: usize,
        operator: &'static str,
        dependency: Dependency,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitError {
    Link(Vec<LinkError>),
    UnsupportedImport {
        dependency: Dependency,
    },
    UnsupportedTableInit {
        table: u32,
    },
    UnsupportedElementExpressions {
        element: u32,
    },
    UnsupportedConstExpr,
    MissingFunction {
        function: u32,
    },
    MissingFunctionBody {
        function: u32,
    },
    MissingType {
        type_index: u32,
    },
    MissingRemap {
        chunk: String,
        dependency: Dependency,
    },
    Parse(String),
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmitError::Link(errors) => {
                write!(f, "link validation failed with {} errors", errors.len())
            }
            EmitError::UnsupportedImport { dependency } => {
                write!(f, "unsupported external import dependency: {dependency:?}")
            }
            EmitError::UnsupportedTableInit { table } => {
                write!(f, "unsupported table init expression for table {table}")
            }
            EmitError::UnsupportedElementExpressions { element } => {
                write!(f, "unsupported expression element segment {element}")
            }
            EmitError::UnsupportedConstExpr => write!(f, "unsupported constant expression"),
            EmitError::MissingFunction { function } => write!(f, "missing function {function}"),
            EmitError::MissingFunctionBody { function } => {
                write!(f, "missing function body for function {function}")
            }
            EmitError::MissingType { type_index } => write!(f, "missing type {type_index}"),
            EmitError::MissingRemap { chunk, dependency } => {
                write!(f, "missing remap for {dependency:?} in chunk {chunk}")
            }
            EmitError::Parse(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for EmitError {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitPlan {
    pub shell: BTreeSet<Dependency>,
    pub routes: Vec<RouteChunkPlan>,
    pub shared: Vec<SharedChunkPlan>,
}

/// Diagnostic record returned alongside an ownership-aware split plan.
///
/// Lets the build pipeline explain *why* code is shaped the way it is —
/// promotions out of routes (cross-route deps the loader can't satisfy),
/// promotions out of shared (loader doesn't pre-fetch shared chunks),
/// and how many table-installed functions had no resolvable owner.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OwnershipReport {
    /// Functions promoted to shell because a route's owned function called
    /// them but they were originally assigned to a different chunk. Each
    /// entry is `(function_index, route_that_needed_it)`.
    pub cross_route_promotions: Vec<(u32, String)>,
    /// Functions promoted to shell because they were assigned to a shared
    /// chunk that the runtime loader can't pre-fetch.
    pub shared_promotions: Vec<u32>,
    /// Functions installed in active table segments that had no name to
    /// classify by. They default to shell, which is safe but blocks
    /// further size wins.
    pub unclassified_table_funcs: Vec<u32>,
}

impl SplitPlan {
    pub fn build_remaps(&self) -> SplitRemapPlan {
        SplitRemapPlan {
            shell: ChunkRemapPlan {
                name: "shell".to_string(),
                dependencies: self.shell.clone(),
                remap: IndexRemap::from_dependencies(&self.shell),
            },
            routes: self
                .routes
                .iter()
                .map(|route| ChunkRemapPlan {
                    name: route.name.clone(),
                    dependencies: route.dependencies.clone(),
                    remap: IndexRemap::from_dependencies(&route.dependencies),
                })
                .collect(),
            shared: self
                .shared
                .iter()
                .map(|shared| ChunkRemapPlan {
                    name: shared_chunk_name(&shared.routes),
                    dependencies: shared.dependencies.clone(),
                    remap: IndexRemap::from_dependencies(&shared.dependencies),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteChunkPlan {
    pub name: String,
    pub required_dependencies: BTreeSet<Dependency>,
    pub dependencies: BTreeSet<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedChunkPlan {
    pub routes: Vec<String>,
    pub dependencies: BTreeSet<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitRemapPlan {
    pub shell: ChunkRemapPlan,
    pub routes: Vec<ChunkRemapPlan>,
    pub shared: Vec<ChunkRemapPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitLinkPlan {
    pub shell: ChunkLinkPlan,
    pub routes: Vec<ChunkLinkPlan>,
    pub shared: Vec<ChunkLinkPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkLinkPlan {
    pub name: String,
    pub owned: BTreeSet<Dependency>,
    pub external: BTreeSet<Dependency>,
    pub local_remap: IndexRemap,
    pub external_remap: IndexRemap,
}

impl ChunkLinkPlan {
    pub fn resolves(&self, dependency: Dependency) -> bool {
        self.local_remap.contains(dependency) || self.external_remap.contains(dependency)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkRemapPlan {
    pub name: String,
    pub dependencies: BTreeSet<Dependency>,
    pub remap: IndexRemap,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexRemap {
    pub functions: BTreeMap<u32, u32>,
    pub types: BTreeMap<u32, u32>,
    pub tables: BTreeMap<u32, u32>,
    pub memories: BTreeMap<u32, u32>,
    pub globals: BTreeMap<u32, u32>,
    pub tags: BTreeMap<u32, u32>,
    pub data: BTreeMap<u32, u32>,
    pub elements: BTreeMap<u32, u32>,
}

impl IndexRemap {
    pub fn from_dependencies(dependencies: &BTreeSet<Dependency>) -> Self {
        let mut remap = Self::default();
        for dependency in dependencies {
            remap.insert(*dependency);
        }
        remap
    }

    pub fn remap(&self, dependency: Dependency) -> Option<u32> {
        self.space(dependency).get(&dependency.index()).copied()
    }

    pub fn contains(&self, dependency: Dependency) -> bool {
        self.remap(dependency).is_some()
    }

    fn insert(&mut self, dependency: Dependency) -> u32 {
        let old_index = dependency.index();
        let space = self.space_mut(dependency);
        if let Some(new_index) = space.get(&old_index) {
            *new_index
        } else {
            let new_index = space.len() as u32;
            space.insert(old_index, new_index);
            new_index
        }
    }

    fn space(&self, dependency: Dependency) -> &BTreeMap<u32, u32> {
        match dependency {
            Dependency::Function(_) => &self.functions,
            Dependency::Type(_) => &self.types,
            Dependency::Table(_) => &self.tables,
            Dependency::Memory(_) => &self.memories,
            Dependency::Global(_) => &self.globals,
            Dependency::Tag(_) => &self.tags,
            Dependency::Data(_) => &self.data,
            Dependency::Element(_) => &self.elements,
        }
    }

    fn space_mut(&mut self, dependency: Dependency) -> &mut BTreeMap<u32, u32> {
        match dependency {
            Dependency::Function(_) => &mut self.functions,
            Dependency::Type(_) => &mut self.types,
            Dependency::Table(_) => &mut self.tables,
            Dependency::Memory(_) => &mut self.memories,
            Dependency::Global(_) => &mut self.globals,
            Dependency::Tag(_) => &mut self.tags,
            Dependency::Data(_) => &mut self.data,
            Dependency::Element(_) => &mut self.elements,
        }
    }
}

impl ModuleAnalysis {
    pub fn validate_indices(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        for function in &self.functions {
            if function.type_index >= self.index_spaces.types {
                errors.push(ValidationError::FunctionTypeIndexOutOfBounds {
                    function: function.index,
                    type_index: function.type_index,
                    limit: self.index_spaces.types,
                });
            }

            for index_use in &function.index_uses {
                if let Some(limit) = self.index_spaces.limit_for(index_use.dependency) {
                    if index_use.dependency.index() >= limit {
                        errors.push(ValidationError::FunctionIndexOutOfBounds {
                            function: function.index,
                            offset: index_use.offset,
                            operator: index_use.operator,
                            dependency: index_use.dependency,
                            limit,
                        });
                    }
                }
            }
        }

        for export in &self.exports {
            if let Some(limit) = self.index_spaces.limit_for_export(export.kind) {
                if export.index >= limit {
                    errors.push(ValidationError::ExportIndexOutOfBounds {
                        export: export.name.clone(),
                        kind: export.kind,
                        index: export.index,
                        limit,
                    });
                }
            }
        }

        for element in &self.elements {
            for dependency in &element.dependencies {
                if let Some(limit) = self.index_spaces.limit_for(*dependency) {
                    if dependency.index() >= limit {
                        errors.push(ValidationError::ElementIndexOutOfBounds {
                            element: element.index,
                            dependency: *dependency,
                            limit,
                        });
                    }
                }
            }
        }

        for global in &self.globals {
            for dependency in &global.dependencies {
                if let Some(limit) = self.index_spaces.limit_for(*dependency) {
                    if dependency.index() >= limit {
                        errors.push(ValidationError::GlobalIndexOutOfBounds {
                            global: global.index,
                            dependency: *dependency,
                            limit,
                        });
                    }
                }
            }
        }

        for data in &self.data_segments {
            for dependency in &data.dependencies {
                if let Some(limit) = self.index_spaces.limit_for(*dependency) {
                    if dependency.index() >= limit {
                        errors.push(ValidationError::DataIndexOutOfBounds {
                            data: data.index,
                            dependency: *dependency,
                            limit,
                        });
                    }
                }
            }
        }

        for tag in &self.tags {
            for dependency in &tag.dependencies {
                if let Some(limit) = self.index_spaces.limit_for(*dependency) {
                    if dependency.index() >= limit {
                        errors.push(ValidationError::TagIndexOutOfBounds {
                            tag: tag.index,
                            dependency: *dependency,
                            limit,
                        });
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn dependency_closure<I>(&self, roots: I) -> Result<BTreeSet<Dependency>, GraphError>
    where
        I: IntoIterator<Item = Dependency>,
    {
        self.closure_with(roots, ClosureMode::Full)
    }

    /// Closure that does NOT auto-expand `Table(t)` → all its element segments.
    ///
    /// `dependency_closure` (Full mode) is correct but over-conservative for
    /// route partitioning: a single `call_indirect` on table 0 records a
    /// `Table(0)` dep, which in Full mode pulls every function installed in
    /// table 0. Direct mode keeps function reachability tight; the partitioner
    /// then layers name-based ownership on top to resolve table-only entries.
    pub fn direct_dependency_closure<I>(&self, roots: I) -> Result<BTreeSet<Dependency>, GraphError>
    where
        I: IntoIterator<Item = Dependency>,
    {
        self.closure_with(roots, ClosureMode::Direct)
    }

    fn closure_with<I>(
        &self,
        roots: I,
        mode: ClosureMode,
    ) -> Result<BTreeSet<Dependency>, GraphError>
    where
        I: IntoIterator<Item = Dependency>,
    {
        self.validate_indices().map_err(GraphError::InvalidModule)?;

        let mut closure = BTreeSet::new();
        let mut stack = Vec::new();

        for root in roots {
            if let Some(limit) = self.index_spaces.limit_for(root) {
                if root.index() >= limit {
                    return Err(GraphError::RootIndexOutOfBounds {
                        dependency: root,
                        limit,
                    });
                }
            }
            stack.push(root);
        }

        while let Some(dependency) = stack.pop() {
            if !closure.insert(dependency) {
                continue;
            }

            match dependency {
                Dependency::Function(index) => {
                    let Some(function) = self.function(index) else {
                        return Err(GraphError::RootIndexOutOfBounds {
                            dependency,
                            limit: self.index_spaces.functions,
                        });
                    };
                    stack.push(Dependency::Type(function.type_index));
                    stack.extend(function.dependencies.iter().copied());
                }
                Dependency::Element(index) => {
                    let Some(element) = self.element(index) else {
                        return Err(GraphError::RootIndexOutOfBounds {
                            dependency,
                            limit: self.index_spaces.elements,
                        });
                    };
                    stack.extend(element.dependencies.iter().copied());
                }
                Dependency::Global(index) => {
                    let Some(global) = self.global(index) else {
                        return Err(GraphError::RootIndexOutOfBounds {
                            dependency,
                            limit: self.index_spaces.globals,
                        });
                    };
                    stack.extend(global.dependencies.iter().copied());
                }
                Dependency::Data(index) => {
                    let Some(data) = self.data(index) else {
                        return Err(GraphError::RootIndexOutOfBounds {
                            dependency,
                            limit: self.index_spaces.data,
                        });
                    };
                    stack.extend(data.dependencies.iter().copied());
                }
                Dependency::Table(index) => {
                    if matches!(mode, ClosureMode::Full) {
                        for element in &self.elements {
                            let ElementKindAnalysis::Active { table_index, .. } = element.kind
                            else {
                                continue;
                            };
                            if table_index.unwrap_or(0) == index {
                                stack.push(Dependency::Element(element.index));
                            }
                        }
                    }
                }
                Dependency::Memory(index) => {
                    for data in &self.data_segments {
                        let DataKindAnalysis::Active { memory_index, .. } = data.kind else {
                            continue;
                        };
                        if memory_index == index {
                            stack.push(Dependency::Data(data.index));
                        }
                    }
                }
                Dependency::Tag(index) => {
                    let Some(tag) = self.tag(index) else {
                        return Err(GraphError::RootIndexOutOfBounds {
                            dependency,
                            limit: self.index_spaces.tags,
                        });
                    };
                    stack.extend(tag.dependencies.iter().copied());
                }
                _ => {}
            }
        }

        Ok(closure)
    }

    pub fn plan_route_split<I>(
        &self,
        shell_roots: I,
        routes: &[RouteSplitRoot],
    ) -> Result<SplitPlan, GraphError>
    where
        I: IntoIterator<Item = Dependency>,
    {
        let mut shell = self.dependency_closure(shell_roots)?;
        let mut usage: BTreeMap<Dependency, BTreeSet<usize>> = BTreeMap::new();
        let mut route_closures = Vec::with_capacity(routes.len());

        for (route_index, route) in routes.iter().enumerate() {
            let closure = self.dependency_closure(route.roots.iter().copied())?;
            for dependency in &closure {
                if !shell.contains(dependency) {
                    usage.entry(*dependency).or_default().insert(route_index);
                }
            }
            route_closures.push(closure);
        }

        let route_count = routes.len();
        for (dependency, route_indices) in &usage {
            if route_count > 0 && route_indices.len() == route_count {
                shell.insert(*dependency);
            }
        }

        let mut route_dependencies = vec![BTreeSet::new(); route_count];
        let mut shared_dependencies: BTreeMap<BTreeSet<usize>, BTreeSet<Dependency>> =
            BTreeMap::new();

        for (dependency, route_indices) in usage {
            if shell.contains(&dependency) {
                continue;
            }

            if route_indices.len() == 1 {
                let route_index = *route_indices.iter().next().expect("route index exists");
                route_dependencies[route_index].insert(dependency);
            } else {
                shared_dependencies
                    .entry(route_indices)
                    .or_default()
                    .insert(dependency);
            }
        }

        let route_plans = routes
            .iter()
            .zip(route_dependencies)
            .zip(route_closures)
            .map(
                |((route, dependencies), required_dependencies)| RouteChunkPlan {
                    name: route.name.clone(),
                    required_dependencies,
                    dependencies,
                },
            )
            .collect();

        let shared = shared_dependencies
            .into_iter()
            .map(|(route_indices, dependencies)| SharedChunkPlan {
                routes: route_indices
                    .into_iter()
                    .map(|index| routes[index].name.clone())
                    .collect(),
                dependencies,
            })
            .collect();

        Ok(SplitPlan {
            shell,
            routes: route_plans,
            shared,
        })
    }

    /// Like `plan_route_split`, but breaks the Table(0)-pulls-everything tie
    /// using mangled-name ownership.
    ///
    /// Direct call edges remain authoritative — anything in shell's direct
    /// closure stays shell, anything in exactly one route's direct closure
    /// belongs to that route. Functions reached only through the shared
    /// funcref table are then assigned by name: a function whose mangled name
    /// contains exactly one `routes::<id>::` substring goes to that route;
    /// multiple matches → shared; none/unknown → shell.
    pub fn plan_route_split_with_ownership<I>(
        &self,
        shell_roots: I,
        routes: &[RouteSplitRoot],
        route_ids: &[String],
    ) -> Result<(SplitPlan, OwnershipReport), GraphError>
    where
        I: IntoIterator<Item = Dependency>,
    {
        let mut report = OwnershipReport::default();

        // Phase 1: direct-only closures. Indirect (table-mediated)
        // reachability is deferred to phase 3, where naming breaks ties.
        let shell_direct = self.direct_dependency_closure(shell_roots)?;
        let mut route_direct = Vec::with_capacity(routes.len());
        for route in routes {
            route_direct.push(self.direct_dependency_closure(route.roots.iter().copied())?);
        }

        // Phase 2: classify deps that live in *some* direct closure.
        let mut shell: BTreeSet<Dependency> = shell_direct.clone();
        let mut route_dependencies: Vec<BTreeSet<Dependency>> = vec![BTreeSet::new(); routes.len()];
        let mut shared_buckets: BTreeMap<BTreeSet<usize>, BTreeSet<Dependency>> = BTreeMap::new();

        let mut all_deps: BTreeSet<Dependency> = shell_direct.iter().copied().collect();
        for closure in &route_direct {
            all_deps.extend(closure.iter().copied());
        }

        for dep in &all_deps {
            if shell_direct.contains(dep) {
                continue; // already in shell
            }
            let users: BTreeSet<usize> = route_direct
                .iter()
                .enumerate()
                .filter_map(|(i, closure)| closure.contains(dep).then_some(i))
                .collect();
            match users.len() {
                0 => {} // unreachable here (would mean dep is not in any closure)
                1 => {
                    let i = *users.iter().next().unwrap();
                    route_dependencies[i].insert(*dep);
                }
                _ => {
                    shared_buckets.entry(users).or_default().insert(*dep);
                }
            }
        }

        // Phase 3: classify table-only functions by name.
        //
        // A function reached only through the shared funcref table doesn't
        // appear in any direct closure. The naive splitter would either pull
        // every such function into every chunk (via Table(0) → element
        // expansion) or pin it to shell. Use mangled names instead.
        //
        // Restrict this to functions actually installed in an active element
        // segment. The synthesized-segment emitter only knows how to move
        // those across chunks; assigning a route owner to a function that's
        // not in any table would just leak it into a route's import list with
        // no way for the route's chunk to satisfy the import.
        let table_installed: BTreeSet<u32> = self
            .elements
            .iter()
            .filter(|e| matches!(e.kind, ElementKindAnalysis::Active { .. }))
            .filter_map(|e| match &e.items {
                ElementItemsAnalysis::Functions(funcs) => Some(funcs.iter().copied()),
                ElementItemsAnalysis::Expressions => None,
            })
            .flatten()
            .collect();

        for function in &self.functions {
            let dep = Dependency::Function(function.index);
            if shell.contains(&dep) || route_dependencies.iter().any(|r| r.contains(&dep)) {
                continue;
            }
            // Skip imported functions and unreachable ones.
            if !function.defined {
                continue;
            }
            // Only consider functions the table actually points at.
            if !table_installed.contains(&function.index) {
                continue;
            }

            let owner = FunctionOwner::from_name(function.name.as_deref(), route_ids);
            match owner {
                FunctionOwner::Unknown => {
                    report.unclassified_table_funcs.push(function.index);
                    shell.insert(dep);
                }
                FunctionOwner::Shell => {
                    shell.insert(dep);
                }
                FunctionOwner::Route(route_id) => {
                    if let Some(i) = routes.iter().position(|r| r.name == route_id) {
                        route_dependencies[i].insert(dep);
                    } else {
                        shell.insert(dep);
                    }
                }
                FunctionOwner::Shared(route_id_set) => {
                    let users: BTreeSet<usize> = route_id_set
                        .iter()
                        .filter_map(|name| routes.iter().position(|r| r.name == *name))
                        .collect();
                    match users.len() {
                        0 => {
                            shell.insert(dep);
                        }
                        1 => {
                            let i = *users.iter().next().unwrap();
                            route_dependencies[i].insert(dep);
                        }
                        _ => {
                            shared_buckets.entry(users).or_default().insert(dep);
                        }
                    }
                }
            }
        }

        // Phase 3.5: enforce the chunk-boundary invariant.
        //
        // Routes import their non-local deps from `pocopine:split`, which the
        // loader resolves to shell's exports. So every direct dep of a
        // route-owned function must live in shell (or in the route itself).
        // Cross-route deps (route_home owns F that calls G owned by route_story)
        // would link-fail: shell can't alias what it doesn't have.
        //
        // Shell faces a stricter version of the same invariant: every direct
        // dep of a shell-owned function must also live in shell, since shell
        // doesn't import from anywhere it could resolve those names. Phase 3's
        // table-installed restriction can leave shell-owned functions whose
        // own deps are unowned (dead-or-table-only), so this loop has to
        // catch them too.
        //
        // Iterate to a fixed point because promoting a function can expose
        // new leaks through that function's own deps.
        loop {
            let mut promoted = false;
            for (route_index, route_deps) in route_dependencies.iter().enumerate() {
                for dep in route_deps {
                    let Dependency::Function(idx) = dep else {
                        continue;
                    };
                    let Some(function) = self.function(*idx) else {
                        continue;
                    };
                    for sub_dep in &function.dependencies {
                        if shell.contains(sub_dep) || route_deps.contains(sub_dep) {
                            continue;
                        }
                        // sub_dep is owned by some other chunk (or unowned).
                        // Promote to shell so this route can import it.
                        if !shell.insert(*sub_dep) {
                            continue;
                        }
                        if let Dependency::Function(sub_idx) = sub_dep {
                            report
                                .cross_route_promotions
                                .push((*sub_idx, routes[route_index].name.clone()));
                        }
                        promoted = true;
                    }
                }
            }
            // Walk shell's own deps too — anything shell-resident with a
            // dangling Function dep needs the callee resident somewhere, and
            // shell can't import from pocopine:split (it *is* pocopine:split).
            let shell_funcs: Vec<u32> = shell
                .iter()
                .filter_map(|d| match d {
                    Dependency::Function(i) => Some(*i),
                    _ => None,
                })
                .collect();
            for idx in shell_funcs {
                let Some(function) = self.function(idx) else {
                    continue;
                };
                for sub_dep in &function.dependencies {
                    if shell.contains(sub_dep) {
                        continue;
                    }
                    // Only Function deps need resident-anywhere; Type/Table/etc.
                    // are imports the chunk emit handles.
                    let Dependency::Function(_) = sub_dep else {
                        continue;
                    };
                    if shell.insert(*sub_dep) {
                        promoted = true;
                    }
                }
            }
            // Drop promoted deps from route_dependencies and shared_buckets.
            for route_deps in route_dependencies.iter_mut() {
                let to_remove: Vec<Dependency> = route_deps
                    .iter()
                    .filter(|d| shell.contains(d))
                    .copied()
                    .collect();
                for d in to_remove {
                    route_deps.remove(&d);
                }
            }
            shared_buckets.retain(|_, deps| {
                deps.retain(|d| !shell.contains(d));
                !deps.is_empty()
            });
            if !promoted {
                break;
            }
        }

        // Phase 3.7: collapse shared chunks into shell.
        //
        // The runtime loader (`pocopine-split-loader.js`) doesn't currently
        // pre-fetch shared chunks before mounting a route, so anything we
        // emitted into a shared chunk wouldn't actually be importable from
        // route code. Promote those deps into shell so the build is correct.
        // When the loader gains shared-chunk support, gate this collapse on a
        // capability flag instead of doing it unconditionally.
        for (_, deps) in std::mem::take(&mut shared_buckets) {
            for dep in deps {
                if shell.insert(dep) {
                    if let Dependency::Function(idx) = dep {
                        report.shared_promotions.push(idx);
                    }
                }
            }
        }

        // Phase 4: assemble the split plan. Required-deps stay tied to direct
        // closures (downstream code uses them to know "what the chunk's roots
        // pulled in"); the dep set is the assignment from phases 2 + 3.
        let route_plans = routes
            .iter()
            .zip(route_dependencies)
            .zip(route_direct.iter())
            .map(
                |((route, dependencies), required_dependencies)| RouteChunkPlan {
                    name: route.name.clone(),
                    required_dependencies: required_dependencies.clone(),
                    dependencies,
                },
            )
            .collect();

        let shared = shared_buckets
            .into_iter()
            .map(|(route_indices, dependencies)| SharedChunkPlan {
                routes: route_indices
                    .into_iter()
                    .map(|index| routes[index].name.clone())
                    .collect(),
                dependencies,
            })
            .collect();

        Ok((
            SplitPlan {
                shell,
                routes: route_plans,
                shared,
            },
            report,
        ))
    }

    pub fn build_link_plan(&self, plan: &SplitPlan) -> SplitLinkPlan {
        SplitLinkPlan {
            shell: self.build_chunk_link_plan("shell".to_string(), &plan.shell, &plan.shell),
            routes: plan
                .routes
                .iter()
                .map(|route| {
                    self.build_chunk_link_plan(
                        route.name.clone(),
                        &route.dependencies,
                        &route.required_dependencies,
                    )
                })
                .collect(),
            shared: plan
                .shared
                .iter()
                .map(|shared| {
                    self.build_chunk_link_plan(
                        shared_chunk_name(&shared.routes),
                        &shared.dependencies,
                        &shared.dependencies,
                    )
                })
                .collect(),
        }
    }

    pub fn validate_link_plan(&self, plan: &SplitLinkPlan) -> Result<(), Vec<LinkError>> {
        let mut errors = Vec::new();
        self.validate_chunk_link_plan(&plan.shell, &mut errors);
        for route in &plan.routes {
            self.validate_chunk_link_plan(route, &mut errors);
        }
        for shared in &plan.shared {
            self.validate_chunk_link_plan(shared, &mut errors);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn emit_function_chunk(&self, chunk: &ChunkLinkPlan) -> Result<Vec<u8>, EmitError> {
        self.validate_chunk_for_emission(chunk)?;

        let mut module = wasm_encoder::Module::new();
        let mut types = wasm_encoder::TypeSection::new();
        for old_type in ordered_indices(&chunk.external_remap.types) {
            self.encode_type(&mut types, old_type)?;
        }
        for old_type in ordered_indices(&chunk.local_remap.types) {
            self.encode_type(&mut types, old_type)?;
        }
        if !types.is_empty() {
            module.section(&types);
        }

        let mut imports = wasm_encoder::ImportSection::new();
        for old_function in ordered_indices(&chunk.external_remap.functions) {
            let function = self
                .function(old_function)
                .ok_or(EmitError::MissingFunction {
                    function: old_function,
                })?;
            let type_index = combined_type_index(chunk, function.type_index)?;
            if let Some(import) = self.function_imports.get(&old_function) {
                imports.import(
                    &import.module,
                    &import.name,
                    wasm_encoder::EntityType::Function(type_index),
                );
            } else {
                imports.import(
                    "pocopine:split",
                    &format!("func:{old_function}"),
                    wasm_encoder::EntityType::Function(type_index),
                );
            }
        }
        for old_table in ordered_indices(&chunk.external_remap.tables) {
            let table = self.table(old_table).ok_or(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency: Dependency::Table(old_table),
            })?;
            let ty = encode_table_type(table.ty)?;
            if let Some(import) = self.table_imports.get(&old_table) {
                imports.import(&import.module, &import.name, ty);
            } else {
                imports.import("pocopine:split", &format!("table:{old_table}"), ty);
            }
        }
        for old_memory in ordered_indices(&chunk.external_remap.memories) {
            let memory = self.memory(old_memory).ok_or(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency: Dependency::Memory(old_memory),
            })?;
            let ty = encode_memory_type(memory.ty);
            if let Some(import) = self.memory_imports.get(&old_memory) {
                imports.import(&import.module, &import.name, ty);
            } else {
                imports.import("pocopine:split", &format!("memory:{old_memory}"), ty);
            }
        }
        for old_global in ordered_indices(&chunk.external_remap.globals) {
            let global = self.global(old_global).ok_or(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency: Dependency::Global(old_global),
            })?;
            let ty = encode_global_type(global.ty)?;
            if let Some(import) = self.global_imports.get(&old_global) {
                imports.import(&import.module, &import.name, ty);
            } else {
                imports.import("pocopine:split", &format!("global:{old_global}"), ty);
            }
        }
        for old_tag in ordered_indices(&chunk.external_remap.tags) {
            let tag = self.tag(old_tag).ok_or(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency: Dependency::Tag(old_tag),
            })?;
            let ty = encode_tag_type(chunk, tag.ty)?;
            if let Some(import) = self.tag_imports.get(&old_tag) {
                imports.import(&import.module, &import.name, ty);
            } else {
                imports.import("pocopine:split", &format!("tag:{old_tag}"), ty);
            }
        }
        if !imports.is_empty() {
            module.section(&imports);
        }

        let mut functions = wasm_encoder::FunctionSection::new();
        let local_functions = ordered_indices(&chunk.local_remap.functions);
        for old_function in &local_functions {
            let function = self
                .function(*old_function)
                .ok_or(EmitError::MissingFunction {
                    function: *old_function,
                })?;
            functions.function(combined_type_index(chunk, function.type_index)?);
        }
        if !functions.is_empty() {
            module.section(&functions);
        }

        let mut tables = wasm_encoder::TableSection::new();
        for old_table in ordered_indices(&chunk.local_remap.tables) {
            let table = self.table(old_table).ok_or(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency: Dependency::Table(old_table),
            })?;
            if table.has_init_expr {
                return Err(EmitError::UnsupportedTableInit { table: old_table });
            }
            tables.table(encode_table_type(table.ty)?);
        }
        if !tables.is_empty() {
            module.section(&tables);
        }

        let mut memories = wasm_encoder::MemorySection::new();
        for old_memory in ordered_indices(&chunk.local_remap.memories) {
            let memory = self.memory(old_memory).ok_or(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency: Dependency::Memory(old_memory),
            })?;
            memories.memory(encode_memory_type(memory.ty));
        }
        if !memories.is_empty() {
            module.section(&memories);
        }

        let mut globals = wasm_encoder::GlobalSection::new();
        for old_global in ordered_indices(&chunk.local_remap.globals) {
            let global = self.global(old_global).ok_or(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency: Dependency::Global(old_global),
            })?;
            let init_expr = global.init_expr.as_ref().ok_or(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency: Dependency::Global(old_global),
            })?;
            globals.global(
                encode_global_type(global.ty)?,
                &encode_const_expr(chunk, init_expr)?,
            );
        }
        if !globals.is_empty() {
            module.section(&globals);
        }

        let mut tags = wasm_encoder::TagSection::new();
        for old_tag in ordered_indices(&chunk.local_remap.tags) {
            let tag = self.tag(old_tag).ok_or(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency: Dependency::Tag(old_tag),
            })?;
            tags.tag(encode_tag_type(chunk, tag.ty)?);
        }
        if !tags.is_empty() {
            module.section(&tags);
        }

        let mut exports = wasm_encoder::ExportSection::new();
        for export in &self.exports {
            let Some((kind, index)) = remap_export(chunk, export)? else {
                continue;
            };
            exports.export(&export.name, kind, index);
        }
        if !exports.is_empty() {
            module.section(&exports);
        }

        let mut elements = wasm_encoder::ElementSection::new();
        let mut covered: BTreeSet<(u32, u32)> = BTreeSet::new(); // (table, slot) emitted via owned segments
        for old_element in ordered_indices(&chunk.local_remap.elements) {
            self.record_covered_slots(old_element, &mut covered);
            self.encode_element(chunk, &mut elements, old_element)?;
        }
        self.encode_synthesized_elements(chunk, &mut elements, &covered)?;
        if !elements.is_empty() {
            module.section(&elements);
        }

        let has_data = !chunk.local_remap.data.is_empty();
        if has_data {
            module.section(&wasm_encoder::DataCountSection {
                count: chunk.local_remap.data.len() as u32,
            });
        }

        let mut code = wasm_encoder::CodeSection::new();
        for old_function in local_functions {
            let function = self
                .function(old_function)
                .ok_or(EmitError::MissingFunction {
                    function: old_function,
                })?;
            let body = function
                .body
                .as_deref()
                .ok_or(EmitError::MissingFunctionBody {
                    function: old_function,
                })?;
            let body = wasmparser::FunctionBody::new(wasmparser::BinaryReader::new(body, 0));
            let mut reencoder = ChunkReencoder { chunk };
            reencoder
                .parse_function_body(&mut code, body)
                .map_err(emit_reencode_error)?;
        }
        if !code.is_empty() {
            module.section(&code);
        }

        let mut data = wasm_encoder::DataSection::new();
        for old_data in ordered_indices(&chunk.local_remap.data) {
            self.encode_data(chunk, &mut data, old_data)?;
        }
        if !data.is_empty() {
            module.section(&data);
        }

        Ok(module.finish())
    }

    fn validate_chunk_for_emission(&self, chunk: &ChunkLinkPlan) -> Result<(), EmitError> {
        if let Err(errors) = self.validate_single_chunk_link(chunk) {
            return Err(EmitError::Link(errors));
        }

        for dependency in &chunk.external {
            match dependency {
                Dependency::Function(_)
                | Dependency::Type(_)
                | Dependency::Table(_)
                | Dependency::Memory(_)
                | Dependency::Global(_)
                | Dependency::Tag(_) => {}
                _ => {
                    return Err(EmitError::UnsupportedImport {
                        dependency: *dependency,
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_single_chunk_link(&self, chunk: &ChunkLinkPlan) -> Result<(), Vec<LinkError>> {
        let mut errors = Vec::new();
        self.validate_chunk_link_plan(chunk, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn encode_type(
        &self,
        types: &mut wasm_encoder::TypeSection,
        old_type: u32,
    ) -> Result<(), EmitError> {
        let ty = self
            .types
            .get(old_type as usize)
            .ok_or(EmitError::MissingType {
                type_index: old_type,
            })?;
        let params = ty
            .params()
            .iter()
            .map(|ty| {
                wasm_encoder::ValType::try_from(*ty)
                    .map_err(|err| EmitError::Parse(err.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let results = ty
            .results()
            .iter()
            .map(|ty| {
                wasm_encoder::ValType::try_from(*ty)
                    .map_err(|err| EmitError::Parse(err.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        types.ty().function(params, results);
        Ok(())
    }

    /// Record (table, slot) pairs that an owned active element segment covers,
    /// so the synthesized-segment pass doesn't double-install at the same slot.
    fn record_covered_slots(&self, old_element: u32, covered: &mut BTreeSet<(u32, u32)>) {
        let Some(element) = self.element(old_element) else {
            return;
        };
        let ElementKindAnalysis::Active {
            table_index,
            offset_expr,
        } = &element.kind
        else {
            return;
        };
        let ConstExprAnalysis::I32Const(base) = offset_expr else {
            return;
        };
        let table = table_index.unwrap_or(0);
        let count = match &element.items {
            ElementItemsAnalysis::Functions(fs) => fs.len() as u32,
            ElementItemsAnalysis::Expressions => return,
        };
        for i in 0..count {
            covered.insert((table, *base as u32 + i));
        }
    }

    /// Synthesize per-chunk active element segments.
    ///
    /// Walks every active source segment installed in table 0 (or wherever)
    /// and, for each entry whose function is owned locally by this chunk,
    /// emits an active install at the original slot. Adjacent slots are
    /// coalesced into a single segment to keep the wasm small.
    ///
    /// This is what makes the ownership-based partition functional: the
    /// monolithic build's single 645-entry segment 0 can't move atomically;
    /// instead each chunk reconstitutes the slots it owns.
    fn encode_synthesized_elements(
        &self,
        chunk: &ChunkLinkPlan,
        elements: &mut wasm_encoder::ElementSection,
        covered: &BTreeSet<(u32, u32)>,
    ) -> Result<(), EmitError> {
        for source in &self.elements {
            let ElementKindAnalysis::Active {
                table_index,
                offset_expr,
            } = &source.kind
            else {
                continue;
            };
            let ConstExprAnalysis::I32Const(base) = offset_expr else {
                continue;
            };
            let ElementItemsAnalysis::Functions(items) = &source.items else {
                continue;
            };
            let table = table_index.unwrap_or(0);
            // Skip tables this chunk can't address.
            if combined_table_index(chunk, table).is_none() {
                continue;
            }

            // Walk entries, batching consecutive locally-owned non-covered slots.
            let mut run: Vec<(u32, u32)> = Vec::new(); // (slot, combined_fn_index)
            let flush = |elements: &mut wasm_encoder::ElementSection,
                         chunk: &ChunkLinkPlan,
                         table: u32,
                         run: &mut Vec<(u32, u32)>|
             -> Result<(), EmitError> {
                if run.is_empty() {
                    return Ok(());
                }
                let base_slot = run[0].0;
                let funcs: Vec<u32> = run.iter().map(|(_, idx)| *idx).collect();
                let offset = wasm_encoder::ConstExpr::i32_const(base_slot as i32);
                let combined_table =
                    combined_table_index(chunk, table).ok_or_else(|| EmitError::MissingRemap {
                        chunk: chunk.name.clone(),
                        dependency: Dependency::Table(table),
                    })?;
                let table_arg = if combined_table == 0 {
                    None
                } else {
                    Some(combined_table)
                };
                elements.active(
                    table_arg,
                    &offset,
                    wasm_encoder::Elements::Functions(Cow::Owned(funcs)),
                );
                run.clear();
                Ok(())
            };

            for (i, fn_idx) in items.iter().enumerate() {
                let slot = *base as u32 + i as u32;
                if covered.contains(&(table, slot)) {
                    flush(elements, chunk, table, &mut run)?;
                    continue;
                }
                // Locally defined functions only — imported-from-elsewhere
                // function indices can't be installed here because the route
                // chunk is loaded after shell, and shell may not have the
                // function at all.
                let Some(local_idx) = chunk.local_remap.functions.get(fn_idx) else {
                    flush(elements, chunk, table, &mut run)?;
                    continue;
                };
                let combined = chunk.external_remap.functions.len() as u32 + *local_idx;
                if !run.is_empty() && run.last().unwrap().0 + 1 != slot {
                    flush(elements, chunk, table, &mut run)?;
                }
                run.push((slot, combined));
            }
            flush(elements, chunk, table, &mut run)?;
        }
        Ok(())
    }

    fn encode_element(
        &self,
        chunk: &ChunkLinkPlan,
        elements: &mut wasm_encoder::ElementSection,
        old_element: u32,
    ) -> Result<(), EmitError> {
        let element = self.element(old_element).ok_or(EmitError::MissingRemap {
            chunk: chunk.name.clone(),
            dependency: Dependency::Element(old_element),
        })?;
        let ElementItemsAnalysis::Functions(functions) = &element.items else {
            return Err(EmitError::UnsupportedElementExpressions {
                element: old_element,
            });
        };

        let remapped_functions = functions
            .iter()
            .map(|function| {
                combined_function_index(chunk, *function).ok_or_else(|| EmitError::MissingRemap {
                    chunk: chunk.name.clone(),
                    dependency: Dependency::Function(*function),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let items = wasm_encoder::Elements::Functions(Cow::Owned(remapped_functions));

        match &element.kind {
            ElementKindAnalysis::Passive => {
                elements.passive(items);
            }
            ElementKindAnalysis::Declared => {
                elements.declared(items);
            }
            ElementKindAnalysis::Active {
                table_index,
                offset_expr,
            } => {
                if matches!(offset_expr, ConstExprAnalysis::Unsupported) {
                    return Err(EmitError::UnsupportedConstExpr);
                }
                let remapped_table = match table_index {
                    Some(table) => Some(combined_table_index(chunk, *table).ok_or_else(|| {
                        EmitError::MissingRemap {
                            chunk: chunk.name.clone(),
                            dependency: Dependency::Table(*table),
                        }
                    })?),
                    None => match combined_table_index(chunk, 0) {
                        Some(0) => None,
                        Some(index) => Some(index),
                        None => {
                            return Err(EmitError::MissingRemap {
                                chunk: chunk.name.clone(),
                                dependency: Dependency::Table(0),
                            });
                        }
                    },
                };
                let offset = encode_const_expr(chunk, offset_expr)?;
                elements.active(remapped_table, &offset, items);
            }
        }
        Ok(())
    }

    fn encode_data(
        &self,
        chunk: &ChunkLinkPlan,
        data: &mut wasm_encoder::DataSection,
        old_data: u32,
    ) -> Result<(), EmitError> {
        let segment = self.data(old_data).ok_or(EmitError::MissingRemap {
            chunk: chunk.name.clone(),
            dependency: Dependency::Data(old_data),
        })?;

        match &segment.kind {
            DataKindAnalysis::Passive => {
                data.passive(segment.data.iter().copied());
            }
            DataKindAnalysis::Active {
                memory_index,
                offset_expr,
            } => {
                let memory = combined_memory_index(chunk, *memory_index).ok_or_else(|| {
                    EmitError::MissingRemap {
                        chunk: chunk.name.clone(),
                        dependency: Dependency::Memory(*memory_index),
                    }
                })?;
                let offset = encode_const_expr(chunk, offset_expr)?;
                data.active(memory, &offset, segment.data.iter().copied());
            }
        }
        Ok(())
    }

    fn build_chunk_link_plan(
        &self,
        name: String,
        owned_dependencies: &BTreeSet<Dependency>,
        _required_dependencies: &BTreeSet<Dependency>,
    ) -> ChunkLinkPlan {
        let owned = owned_dependencies
            .difference(&self.imports)
            .copied()
            .collect::<BTreeSet<_>>();
        let mut external = owned_dependencies
            .intersection(&self.imports)
            .copied()
            .collect::<BTreeSet<_>>();

        for dependency in &owned {
            let Dependency::Function(index) = dependency else {
                continue;
            };
            let Some(function) = self.function(*index) else {
                continue;
            };

            let type_dependency = Dependency::Type(function.type_index);
            if !owned.contains(&type_dependency) {
                external.insert(type_dependency);
            }

            for function_dependency in &function.dependencies {
                if !owned.contains(function_dependency) {
                    external.insert(*function_dependency);
                }
            }
        }

        for dependency in &owned {
            let Dependency::Element(index) = dependency else {
                continue;
            };
            let Some(element) = self.element(*index) else {
                continue;
            };
            for element_dependency in &element.dependencies {
                if !owned.contains(element_dependency) {
                    external.insert(*element_dependency);
                }
            }
        }

        for dependency in &owned {
            let Dependency::Global(index) = dependency else {
                continue;
            };
            let Some(global) = self.global(*index) else {
                continue;
            };
            for global_dependency in &global.dependencies {
                if !owned.contains(global_dependency) {
                    external.insert(*global_dependency);
                }
            }
        }

        for dependency in &owned {
            let Dependency::Data(index) = dependency else {
                continue;
            };
            let Some(data) = self.data(*index) else {
                continue;
            };
            for data_dependency in &data.dependencies {
                if !owned.contains(data_dependency) {
                    external.insert(*data_dependency);
                }
            }
        }

        for dependency in &owned {
            let Dependency::Tag(index) = dependency else {
                continue;
            };
            let Some(tag) = self.tag(*index) else {
                continue;
            };
            for tag_dependency in &tag.dependencies {
                if !owned.contains(tag_dependency) {
                    external.insert(*tag_dependency);
                }
            }
        }

        let external_dependencies = external.iter().copied().collect::<Vec<_>>();
        for dependency in external_dependencies {
            match dependency {
                Dependency::Function(index) => {
                    let Some(function) = self.function(index) else {
                        continue;
                    };
                    external.insert(Dependency::Type(function.type_index));
                }
                Dependency::Tag(index) => {
                    let Some(tag) = self.tag(index) else {
                        continue;
                    };
                    external.insert(Dependency::Type(tag.ty.func_type_idx));
                }
                _ => {}
            }
        }

        ChunkLinkPlan {
            name,
            local_remap: IndexRemap::from_dependencies(&owned),
            external_remap: IndexRemap::from_dependencies(&external),
            owned,
            external,
        }
    }

    fn validate_chunk_link_plan(&self, chunk: &ChunkLinkPlan, errors: &mut Vec<LinkError>) {
        for dependency in &chunk.owned {
            let Dependency::Function(index) = dependency else {
                continue;
            };
            let Some(function) = self.function(*index) else {
                continue;
            };

            let type_dependency = Dependency::Type(function.type_index);
            if !chunk.resolves(type_dependency) {
                errors.push(LinkError::MissingFunctionTypeRemap {
                    chunk: chunk.name.clone(),
                    function: function.index,
                    type_index: function.type_index,
                });
            }

            for index_use in &function.index_uses {
                if !chunk.resolves(index_use.dependency) {
                    errors.push(LinkError::MissingInstructionRemap {
                        chunk: chunk.name.clone(),
                        function: function.index,
                        offset: index_use.offset,
                        operator: index_use.operator,
                        dependency: index_use.dependency,
                    });
                }
            }
        }
    }

    fn function(&self, index: u32) -> Option<&FunctionAnalysis> {
        self.functions
            .get(index as usize)
            .filter(|function| function.index == index)
    }

    fn table(&self, index: u32) -> Option<&TableAnalysis> {
        self.tables
            .get(index as usize)
            .filter(|table| table.index == index)
    }

    fn element(&self, index: u32) -> Option<&ElementAnalysis> {
        self.elements
            .get(index as usize)
            .filter(|element| element.index == index)
    }

    fn global(&self, index: u32) -> Option<&GlobalAnalysis> {
        self.globals
            .get(index as usize)
            .filter(|global| global.index == index)
    }

    fn memory(&self, index: u32) -> Option<&MemoryAnalysis> {
        self.memories
            .get(index as usize)
            .filter(|memory| memory.index == index)
    }

    fn data(&self, index: u32) -> Option<&DataAnalysis> {
        self.data_segments
            .get(index as usize)
            .filter(|data| data.index == index)
    }

    fn tag(&self, index: u32) -> Option<&TagAnalysis> {
        self.tags
            .get(index as usize)
            .filter(|tag| tag.index == index)
    }
}

impl Dependency {
    fn index(self) -> u32 {
        match self {
            Dependency::Function(index)
            | Dependency::Type(index)
            | Dependency::Table(index)
            | Dependency::Memory(index)
            | Dependency::Global(index)
            | Dependency::Tag(index)
            | Dependency::Data(index)
            | Dependency::Element(index) => index,
        }
    }
}

impl IndexSpaces {
    fn limit_for(self, dependency: Dependency) -> Option<u32> {
        Some(match dependency {
            Dependency::Function(_) => self.functions,
            Dependency::Type(_) => self.types,
            Dependency::Table(_) => self.tables,
            Dependency::Memory(_) => self.memories,
            Dependency::Global(_) => self.globals,
            Dependency::Tag(_) => self.tags,
            Dependency::Data(_) => self.data,
            Dependency::Element(_) => self.elements,
        })
    }

    fn limit_for_export(self, kind: ExternalKind) -> Option<u32> {
        match kind {
            ExternalKind::Func => Some(self.functions),
            ExternalKind::Table => Some(self.tables),
            ExternalKind::Memory => Some(self.memories),
            ExternalKind::Global => Some(self.globals),
            ExternalKind::Tag => Some(self.tags),
        }
    }
}

fn shared_chunk_name(routes: &[String]) -> String {
    format!("shared:{}", routes.join("+"))
}

fn ordered_indices(remap: &BTreeMap<u32, u32>) -> Vec<u32> {
    let mut pairs = remap
        .iter()
        .map(|(old_index, new_index)| (*old_index, *new_index))
        .collect::<Vec<_>>();
    pairs.sort_by_key(|(_, new_index)| *new_index);
    pairs.into_iter().map(|(old_index, _)| old_index).collect()
}

fn remap_export(
    chunk: &ChunkLinkPlan,
    export: &ExportAnalysis,
) -> Result<Option<(wasm_encoder::ExportKind, u32)>, EmitError> {
    match export.kind {
        ExternalKind::Func if chunk.local_remap.functions.contains_key(&export.index) => {
            let index = combined_function_index(chunk, export.index).ok_or_else(|| {
                EmitError::MissingRemap {
                    chunk: chunk.name.clone(),
                    dependency: Dependency::Function(export.index),
                }
            })?;
            Ok(Some((wasm_encoder::ExportKind::Func, index)))
        }
        ExternalKind::Table if chunk.local_remap.tables.contains_key(&export.index) => {
            let index = combined_table_index(chunk, export.index).ok_or_else(|| {
                EmitError::MissingRemap {
                    chunk: chunk.name.clone(),
                    dependency: Dependency::Table(export.index),
                }
            })?;
            Ok(Some((wasm_encoder::ExportKind::Table, index)))
        }
        ExternalKind::Memory if chunk.local_remap.memories.contains_key(&export.index) => {
            let index = combined_memory_index(chunk, export.index).ok_or_else(|| {
                EmitError::MissingRemap {
                    chunk: chunk.name.clone(),
                    dependency: Dependency::Memory(export.index),
                }
            })?;
            Ok(Some((wasm_encoder::ExportKind::Memory, index)))
        }
        ExternalKind::Global if chunk.local_remap.globals.contains_key(&export.index) => {
            let index = combined_global_index(chunk, export.index).ok_or_else(|| {
                EmitError::MissingRemap {
                    chunk: chunk.name.clone(),
                    dependency: Dependency::Global(export.index),
                }
            })?;
            Ok(Some((wasm_encoder::ExportKind::Global, index)))
        }
        ExternalKind::Tag if chunk.local_remap.tags.contains_key(&export.index) => {
            let index =
                combined_tag_index(chunk, export.index).ok_or_else(|| EmitError::MissingRemap {
                    chunk: chunk.name.clone(),
                    dependency: Dependency::Tag(export.index),
                })?;
            Ok(Some((wasm_encoder::ExportKind::Tag, index)))
        }
        _ => Ok(None),
    }
}

fn combined_function_index(chunk: &ChunkLinkPlan, old_index: u32) -> Option<u32> {
    if let Some(index) = chunk.external_remap.functions.get(&old_index) {
        Some(*index)
    } else {
        chunk
            .local_remap
            .functions
            .get(&old_index)
            .map(|index| chunk.external_remap.functions.len() as u32 + *index)
    }
}

fn combined_table_index(chunk: &ChunkLinkPlan, old_index: u32) -> Option<u32> {
    if let Some(index) = chunk.external_remap.tables.get(&old_index) {
        Some(*index)
    } else {
        chunk
            .local_remap
            .tables
            .get(&old_index)
            .map(|index| chunk.external_remap.tables.len() as u32 + *index)
    }
}

fn combined_memory_index(chunk: &ChunkLinkPlan, old_index: u32) -> Option<u32> {
    if let Some(index) = chunk.external_remap.memories.get(&old_index) {
        Some(*index)
    } else {
        chunk
            .local_remap
            .memories
            .get(&old_index)
            .map(|index| chunk.external_remap.memories.len() as u32 + *index)
    }
}

fn combined_global_index(chunk: &ChunkLinkPlan, old_index: u32) -> Option<u32> {
    if let Some(index) = chunk.external_remap.globals.get(&old_index) {
        Some(*index)
    } else {
        chunk
            .local_remap
            .globals
            .get(&old_index)
            .map(|index| chunk.external_remap.globals.len() as u32 + *index)
    }
}

fn combined_tag_index(chunk: &ChunkLinkPlan, old_index: u32) -> Option<u32> {
    if let Some(index) = chunk.external_remap.tags.get(&old_index) {
        Some(*index)
    } else {
        chunk
            .local_remap
            .tags
            .get(&old_index)
            .map(|index| chunk.external_remap.tags.len() as u32 + *index)
    }
}

fn combined_data_index(chunk: &ChunkLinkPlan, old_index: u32) -> Option<u32> {
    chunk.local_remap.data.get(&old_index).copied()
}

fn combined_element_index(chunk: &ChunkLinkPlan, old_index: u32) -> Option<u32> {
    if let Some(index) = chunk.external_remap.elements.get(&old_index) {
        Some(*index)
    } else {
        chunk
            .local_remap
            .elements
            .get(&old_index)
            .map(|index| chunk.external_remap.elements.len() as u32 + *index)
    }
}

fn combined_type_index(chunk: &ChunkLinkPlan, old_index: u32) -> Result<u32, EmitError> {
    if let Some(index) = chunk.external_remap.types.get(&old_index) {
        Ok(*index)
    } else if let Some(index) = chunk.local_remap.types.get(&old_index) {
        Ok(chunk.external_remap.types.len() as u32 + *index)
    } else {
        Err(EmitError::MissingRemap {
            chunk: chunk.name.clone(),
            dependency: Dependency::Type(old_index),
        })
    }
}

fn combined_required_index(
    chunk: &ChunkLinkPlan,
    dependency: Dependency,
) -> Result<u32, ReencodeError<EmitError>> {
    match dependency {
        Dependency::Function(index) => combined_function_index(chunk, index).ok_or_else(|| {
            ReencodeError::UserError(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency,
            })
        }),
        Dependency::Type(index) => {
            combined_type_index(chunk, index).map_err(ReencodeError::UserError)
        }
        Dependency::Table(index) => combined_table_index(chunk, index).ok_or_else(|| {
            ReencodeError::UserError(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency,
            })
        }),
        Dependency::Memory(index) => combined_memory_index(chunk, index).ok_or_else(|| {
            ReencodeError::UserError(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency,
            })
        }),
        Dependency::Element(index) => combined_element_index(chunk, index).ok_or_else(|| {
            ReencodeError::UserError(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency,
            })
        }),
        Dependency::Global(index) => combined_global_index(chunk, index).ok_or_else(|| {
            ReencodeError::UserError(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency,
            })
        }),
        Dependency::Data(index) => combined_data_index(chunk, index).ok_or_else(|| {
            ReencodeError::UserError(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency,
            })
        }),
        Dependency::Tag(index) => combined_tag_index(chunk, index).ok_or_else(|| {
            ReencodeError::UserError(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency,
            })
        }),
    }
}

fn encode_table_type(ty: wasmparser::TableType) -> Result<wasm_encoder::TableType, EmitError> {
    wasm_encoder::TableType::try_from(ty).map_err(|err| EmitError::Parse(err.to_string()))
}

fn encode_global_type(ty: wasmparser::GlobalType) -> Result<wasm_encoder::GlobalType, EmitError> {
    wasm_encoder::GlobalType::try_from(ty).map_err(|err| EmitError::Parse(err.to_string()))
}

fn encode_memory_type(ty: wasmparser::MemoryType) -> wasm_encoder::MemoryType {
    ty.into()
}

fn encode_tag_type(
    chunk: &ChunkLinkPlan,
    ty: wasmparser::TagType,
) -> Result<wasm_encoder::TagType, EmitError> {
    let kind = match ty.kind {
        wasmparser::TagKind::Exception => wasm_encoder::TagKind::Exception,
    };
    Ok(wasm_encoder::TagType {
        kind,
        func_type_idx: combined_type_index(chunk, ty.func_type_idx)?,
    })
}

fn encode_const_expr(
    chunk: &ChunkLinkPlan,
    expr: &ConstExprAnalysis,
) -> Result<wasm_encoder::ConstExpr, EmitError> {
    match expr {
        ConstExprAnalysis::I32Const(value) => Ok(wasm_encoder::ConstExpr::i32_const(*value)),
        ConstExprAnalysis::GlobalGet(global) => {
            let global =
                combined_global_index(chunk, *global).ok_or_else(|| EmitError::MissingRemap {
                    chunk: chunk.name.clone(),
                    dependency: Dependency::Global(*global),
                })?;
            Ok(wasm_encoder::ConstExpr::global_get(global))
        }
        ConstExprAnalysis::Unsupported => Err(EmitError::UnsupportedConstExpr),
    }
}

fn emit_reencode_error(error: ReencodeError<EmitError>) -> EmitError {
    match error {
        ReencodeError::UserError(error) => error,
        other => EmitError::Parse(other.to_string()),
    }
}

struct ChunkReencoder<'a> {
    chunk: &'a ChunkLinkPlan,
}

impl Reencode for ChunkReencoder<'_> {
    type Error = EmitError;

    fn function_index(&mut self, func: u32) -> Result<u32, ReencodeError<Self::Error>> {
        combined_required_index(self.chunk, Dependency::Function(func))
    }

    fn type_index(&mut self, ty: u32) -> Result<u32, ReencodeError<Self::Error>> {
        combined_required_index(self.chunk, Dependency::Type(ty))
    }

    fn table_index(&mut self, table: u32) -> Result<u32, ReencodeError<Self::Error>> {
        combined_required_index(self.chunk, Dependency::Table(table))
    }

    fn memory_index(&mut self, memory: u32) -> Result<u32, ReencodeError<Self::Error>> {
        combined_required_index(self.chunk, Dependency::Memory(memory))
    }

    fn global_index(&mut self, global: u32) -> Result<u32, ReencodeError<Self::Error>> {
        combined_required_index(self.chunk, Dependency::Global(global))
    }

    fn tag_index(&mut self, tag: u32) -> Result<u32, ReencodeError<Self::Error>> {
        combined_required_index(self.chunk, Dependency::Tag(tag))
    }

    fn data_index(&mut self, data: u32) -> Result<u32, ReencodeError<Self::Error>> {
        combined_required_index(self.chunk, Dependency::Data(data))
    }

    fn element_index(&mut self, element: u32) -> Result<u32, ReencodeError<Self::Error>> {
        combined_required_index(self.chunk, Dependency::Element(element))
    }
}

pub fn analyze(wasm: &[u8]) -> WasmResult<ModuleAnalysis> {
    let mut analysis = ModuleAnalysis::default();
    let mut imported_function_types = Vec::new();
    let mut defined_function_types = Vec::new();
    let mut defined_function_bodies = Vec::new();
    let mut function_names: BTreeMap<u32, String> = BTreeMap::new();

    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::TypeSection(reader) => {
                analysis.types = reader
                    .into_iter_err_on_gc_types()
                    .collect::<Result<Vec<_>, _>>()?;
                analysis.index_spaces.types = analysis.types.len() as u32;
            }
            Payload::ImportSection(reader) => {
                for import in reader {
                    let import = import?;
                    match import.ty {
                        TypeRef::Func(type_index) => {
                            let function = imported_function_types.len() as u32;
                            analysis.imports.insert(Dependency::Function(function));
                            analysis.function_imports.insert(
                                function,
                                FunctionImport {
                                    function,
                                    module: import.module.to_string(),
                                    name: import.name.to_string(),
                                    type_index,
                                },
                            );
                            imported_function_types.push(type_index);
                        }
                        TypeRef::Table(ty) => {
                            let table = analysis.index_spaces.tables;
                            analysis.imports.insert(Dependency::Table(table));
                            analysis.table_imports.insert(
                                table,
                                TableImport {
                                    table,
                                    module: import.module.to_string(),
                                    name: import.name.to_string(),
                                },
                            );
                            analysis.tables.push(TableAnalysis {
                                index: table,
                                ty,
                                imported: true,
                                has_init_expr: false,
                            });
                            analysis.index_spaces.tables += 1;
                        }
                        TypeRef::Memory(ty) => {
                            let memory = analysis.index_spaces.memories;
                            analysis.imports.insert(Dependency::Memory(memory));
                            analysis.memory_imports.insert(
                                memory,
                                MemoryImport {
                                    memory,
                                    module: import.module.to_string(),
                                    name: import.name.to_string(),
                                },
                            );
                            analysis.memories.push(MemoryAnalysis {
                                index: memory,
                                ty,
                                imported: true,
                            });
                            analysis.index_spaces.memories += 1;
                        }
                        TypeRef::Global(ty) => {
                            let global = analysis.index_spaces.globals;
                            analysis.imports.insert(Dependency::Global(global));
                            analysis.global_imports.insert(
                                global,
                                GlobalImport {
                                    global,
                                    module: import.module.to_string(),
                                    name: import.name.to_string(),
                                },
                            );
                            analysis.globals.push(GlobalAnalysis {
                                index: global,
                                ty,
                                imported: true,
                                init_expr: None,
                                dependencies: BTreeSet::new(),
                            });
                            analysis.index_spaces.globals += 1;
                        }
                        TypeRef::Tag(ty) => {
                            let tag = analysis.index_spaces.tags;
                            analysis.imports.insert(Dependency::Tag(tag));
                            analysis.tag_imports.insert(
                                tag,
                                TagImport {
                                    tag,
                                    module: import.module.to_string(),
                                    name: import.name.to_string(),
                                },
                            );
                            analysis.tags.push(TagAnalysis {
                                index: tag,
                                ty,
                                imported: true,
                                dependencies: BTreeSet::from([Dependency::Type(ty.func_type_idx)]),
                            });
                            analysis.index_spaces.tags += 1;
                        }
                    }
                }
            }
            Payload::FunctionSection(reader) => {
                defined_function_types = reader.into_iter().collect::<Result<Vec<_>, _>>()?;
            }
            Payload::CodeSectionEntry(body) => {
                defined_function_bodies.push(scan_function_body(&body)?);
            }
            Payload::TableSection(reader) => {
                for table in reader {
                    let table = table?;
                    let index = analysis.index_spaces.tables;
                    analysis.tables.push(TableAnalysis {
                        index,
                        ty: table.ty,
                        imported: false,
                        has_init_expr: !matches!(table.init, wasmparser::TableInit::RefNull),
                    });
                    analysis.index_spaces.tables += 1;
                }
            }
            Payload::MemorySection(reader) => {
                for memory in reader {
                    let memory = memory?;
                    let index = analysis.index_spaces.memories;
                    analysis.memories.push(MemoryAnalysis {
                        index,
                        ty: memory,
                        imported: false,
                    });
                    analysis.index_spaces.memories += 1;
                }
            }
            Payload::GlobalSection(reader) => {
                for global in reader {
                    let global = global?;
                    let index = analysis.index_spaces.globals;
                    let init_expr = analyze_const_expr(global.init_expr);
                    let mut dependencies = BTreeSet::new();
                    if let ConstExprAnalysis::GlobalGet(global_index) = init_expr {
                        dependencies.insert(Dependency::Global(global_index));
                    }
                    analysis.globals.push(GlobalAnalysis {
                        index,
                        ty: global.ty,
                        imported: false,
                        init_expr: Some(init_expr),
                        dependencies,
                    });
                    analysis.index_spaces.globals += 1;
                }
            }
            Payload::TagSection(reader) => {
                for tag in reader {
                    let tag = tag?;
                    let index = analysis.index_spaces.tags;
                    analysis.tags.push(TagAnalysis {
                        index,
                        ty: tag,
                        imported: false,
                        dependencies: BTreeSet::from([Dependency::Type(tag.func_type_idx)]),
                    });
                    analysis.index_spaces.tags += 1;
                }
            }
            Payload::ElementSection(reader) => {
                for element in reader {
                    let index = analysis.index_spaces.elements;
                    analysis.elements.push(analyze_element(index, element?)?);
                    analysis.index_spaces.elements += 1;
                }
            }
            Payload::DataSection(reader) => {
                for data in reader {
                    let index = analysis.index_spaces.data;
                    analysis.data_segments.push(analyze_data(index, data?)?);
                    analysis.index_spaces.data += 1;
                }
            }
            Payload::DataCountSection { .. } => {}
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export?;
                    analysis.exports.push(ExportAnalysis {
                        name: export.name.to_string(),
                        kind: export.kind,
                        index: export.index,
                    });
                }
            }
            Payload::CustomSection(reader) if reader.name() == "name" => {
                if let wasmparser::KnownCustom::Name(names) = reader.as_known() {
                    for subsection in names {
                        let Ok(subsection) = subsection else { continue };
                        if let wasmparser::Name::Function(map) = subsection {
                            for naming in map {
                                let Ok(naming) = naming else { continue };
                                function_names.insert(naming.index, naming.name.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    analysis.imported_functions = imported_function_types.len() as u32;
    analysis
        .functions
        .extend(
            imported_function_types
                .into_iter()
                .enumerate()
                .map(|(index, type_index)| FunctionAnalysis {
                    index: index as u32,
                    type_index,
                    defined: false,
                    body: None,
                    index_uses: Vec::new(),
                    dependencies: BTreeSet::new(),
                    name: None,
                }),
        );

    for (defined_index, type_index) in defined_function_types.into_iter().enumerate() {
        let index = analysis.imported_functions + defined_index as u32;
        let mut function = defined_function_bodies
            .get(defined_index)
            .cloned()
            .unwrap_or_else(|| empty_defined_function(index, type_index));
        function.index = index;
        function.type_index = type_index;
        analysis.functions.push(function);
    }
    analysis.index_spaces.functions = analysis.functions.len() as u32;

    for function in &mut analysis.functions {
        function.name = function_names.remove(&function.index);
    }

    Ok(analysis)
}

fn analyze_data(index: u32, data: wasmparser::Data<'_>) -> WasmResult<DataAnalysis> {
    let mut dependencies = BTreeSet::new();
    let kind = match data.kind {
        wasmparser::DataKind::Passive => DataKindAnalysis::Passive,
        wasmparser::DataKind::Active {
            memory_index,
            offset_expr,
        } => {
            dependencies.insert(Dependency::Memory(memory_index));
            let offset_expr = analyze_const_expr(offset_expr);
            if let ConstExprAnalysis::GlobalGet(global_index) = offset_expr {
                dependencies.insert(Dependency::Global(global_index));
            }
            DataKindAnalysis::Active {
                memory_index,
                offset_expr,
            }
        }
    };

    Ok(DataAnalysis {
        index,
        kind,
        data: data.data.to_vec(),
        dependencies,
    })
}

fn analyze_element(index: u32, element: wasmparser::Element<'_>) -> WasmResult<ElementAnalysis> {
    let mut dependencies = BTreeSet::new();
    let kind = match element.kind {
        wasmparser::ElementKind::Passive => ElementKindAnalysis::Passive,
        wasmparser::ElementKind::Declared => ElementKindAnalysis::Declared,
        wasmparser::ElementKind::Active {
            table_index,
            offset_expr,
        } => {
            dependencies.insert(Dependency::Table(table_index.unwrap_or(0)));
            ElementKindAnalysis::Active {
                table_index,
                offset_expr: analyze_const_expr(offset_expr),
            }
        }
    };

    let items = match element.items {
        wasmparser::ElementItems::Functions(functions) => {
            let functions = functions.into_iter().collect::<Result<Vec<_>, _>>()?;
            dependencies.extend(functions.iter().copied().map(Dependency::Function));
            ElementItemsAnalysis::Functions(functions)
        }
        wasmparser::ElementItems::Expressions(_, _) => ElementItemsAnalysis::Expressions,
    };

    Ok(ElementAnalysis {
        index,
        kind,
        items,
        dependencies,
    })
}

fn analyze_const_expr(expr: wasmparser::ConstExpr<'_>) -> ConstExprAnalysis {
    let mut ops = expr.get_operators_reader();
    let Ok(first) = ops.read() else {
        return ConstExprAnalysis::Unsupported;
    };
    let out = match first {
        Operator::I32Const { value } => ConstExprAnalysis::I32Const(value),
        Operator::GlobalGet { global_index } => ConstExprAnalysis::GlobalGet(global_index),
        _ => return ConstExprAnalysis::Unsupported,
    };
    match ops.read() {
        Ok(Operator::End) => out,
        _ => ConstExprAnalysis::Unsupported,
    }
}

fn empty_defined_function(index: u32, type_index: u32) -> FunctionAnalysis {
    FunctionAnalysis {
        index,
        type_index,
        defined: true,
        body: None,
        index_uses: Vec::new(),
        dependencies: BTreeSet::new(),
        name: None,
    }
}

fn scan_function_body(body: &wasmparser::FunctionBody<'_>) -> WasmResult<FunctionAnalysis> {
    let mut function = FunctionAnalysis {
        index: 0,
        type_index: 0,
        defined: true,
        body: Some(body.as_bytes().to_vec()),
        index_uses: Vec::new(),
        dependencies: BTreeSet::new(),
        name: None,
    };
    let mut reader = body.get_operators_reader()?;
    while !reader.eof() {
        let offset = reader.original_position();
        let op = reader.read()?;
        record_operator_dependencies(&mut function, offset, &op);
    }
    Ok(function)
}

fn record(
    function: &mut FunctionAnalysis,
    offset: usize,
    dependency: Dependency,
    operator: &'static str,
) {
    function.index_uses.push(IndexUse {
        offset,
        dependency,
        operator,
    });
    function.dependencies.insert(dependency);
}

fn record_operator_dependencies(function: &mut FunctionAnalysis, offset: usize, op: &Operator<'_>) {
    match op {
        Operator::Call { function_index } => {
            record(
                function,
                offset,
                Dependency::Function(*function_index),
                "call",
            );
        }
        Operator::ReturnCall { function_index } => {
            record(
                function,
                offset,
                Dependency::Function(*function_index),
                "return_call",
            );
        }
        Operator::RefFunc { function_index } => {
            record(
                function,
                offset,
                Dependency::Function(*function_index),
                "ref_func",
            );
        }
        Operator::CallIndirect {
            type_index,
            table_index,
            ..
        } => {
            record(
                function,
                offset,
                Dependency::Type(*type_index),
                "call_indirect",
            );
            record(
                function,
                offset,
                Dependency::Table(*table_index),
                "call_indirect",
            );
        }
        Operator::ReturnCallIndirect {
            type_index,
            table_index,
            ..
        } => {
            record(
                function,
                offset,
                Dependency::Type(*type_index),
                "return_call_indirect",
            );
            record(
                function,
                offset,
                Dependency::Table(*table_index),
                "return_call_indirect",
            );
        }
        Operator::GlobalGet { global_index } => {
            record(
                function,
                offset,
                Dependency::Global(*global_index),
                "global_get",
            );
        }
        Operator::GlobalSet { global_index } => {
            record(
                function,
                offset,
                Dependency::Global(*global_index),
                "global_set",
            );
        }
        Operator::I32Load { memarg }
        | Operator::I64Load { memarg }
        | Operator::F32Load { memarg }
        | Operator::F64Load { memarg }
        | Operator::I32Load8S { memarg }
        | Operator::I32Load8U { memarg }
        | Operator::I32Load16S { memarg }
        | Operator::I32Load16U { memarg }
        | Operator::I64Load8S { memarg }
        | Operator::I64Load8U { memarg }
        | Operator::I64Load16S { memarg }
        | Operator::I64Load16U { memarg }
        | Operator::I64Load32S { memarg }
        | Operator::I64Load32U { memarg }
        | Operator::I32Store { memarg }
        | Operator::I64Store { memarg }
        | Operator::F32Store { memarg }
        | Operator::F64Store { memarg }
        | Operator::I32Store8 { memarg }
        | Operator::I32Store16 { memarg }
        | Operator::I64Store8 { memarg }
        | Operator::I64Store16 { memarg }
        | Operator::I64Store32 { memarg }
        | Operator::MemoryAtomicNotify { memarg }
        | Operator::MemoryAtomicWait32 { memarg }
        | Operator::MemoryAtomicWait64 { memarg }
        | Operator::I32AtomicLoad { memarg }
        | Operator::I64AtomicLoad { memarg }
        | Operator::I32AtomicLoad8U { memarg }
        | Operator::I32AtomicLoad16U { memarg }
        | Operator::I64AtomicLoad8U { memarg }
        | Operator::I64AtomicLoad16U { memarg }
        | Operator::I64AtomicLoad32U { memarg }
        | Operator::I32AtomicStore { memarg }
        | Operator::I64AtomicStore { memarg }
        | Operator::I32AtomicStore8 { memarg }
        | Operator::I32AtomicStore16 { memarg }
        | Operator::I64AtomicStore8 { memarg }
        | Operator::I64AtomicStore16 { memarg }
        | Operator::I64AtomicStore32 { memarg }
        | Operator::I32AtomicRmwAdd { memarg }
        | Operator::I64AtomicRmwAdd { memarg }
        | Operator::I32AtomicRmw8AddU { memarg }
        | Operator::I32AtomicRmw16AddU { memarg }
        | Operator::I64AtomicRmw8AddU { memarg }
        | Operator::I64AtomicRmw16AddU { memarg }
        | Operator::I64AtomicRmw32AddU { memarg }
        | Operator::I32AtomicRmwSub { memarg }
        | Operator::I64AtomicRmwSub { memarg }
        | Operator::I32AtomicRmw8SubU { memarg }
        | Operator::I32AtomicRmw16SubU { memarg }
        | Operator::I64AtomicRmw8SubU { memarg }
        | Operator::I64AtomicRmw16SubU { memarg }
        | Operator::I64AtomicRmw32SubU { memarg }
        | Operator::I32AtomicRmwAnd { memarg }
        | Operator::I64AtomicRmwAnd { memarg }
        | Operator::I32AtomicRmw8AndU { memarg }
        | Operator::I32AtomicRmw16AndU { memarg }
        | Operator::I64AtomicRmw8AndU { memarg }
        | Operator::I64AtomicRmw16AndU { memarg }
        | Operator::I64AtomicRmw32AndU { memarg }
        | Operator::I32AtomicRmwOr { memarg }
        | Operator::I64AtomicRmwOr { memarg }
        | Operator::I32AtomicRmw8OrU { memarg }
        | Operator::I32AtomicRmw16OrU { memarg }
        | Operator::I64AtomicRmw8OrU { memarg }
        | Operator::I64AtomicRmw16OrU { memarg }
        | Operator::I64AtomicRmw32OrU { memarg }
        | Operator::I32AtomicRmwXor { memarg }
        | Operator::I64AtomicRmwXor { memarg }
        | Operator::I32AtomicRmw8XorU { memarg }
        | Operator::I32AtomicRmw16XorU { memarg }
        | Operator::I64AtomicRmw8XorU { memarg }
        | Operator::I64AtomicRmw16XorU { memarg }
        | Operator::I64AtomicRmw32XorU { memarg }
        | Operator::I32AtomicRmwXchg { memarg }
        | Operator::I64AtomicRmwXchg { memarg }
        | Operator::I32AtomicRmw8XchgU { memarg }
        | Operator::I32AtomicRmw16XchgU { memarg }
        | Operator::I64AtomicRmw8XchgU { memarg }
        | Operator::I64AtomicRmw16XchgU { memarg }
        | Operator::I64AtomicRmw32XchgU { memarg }
        | Operator::I32AtomicRmwCmpxchg { memarg }
        | Operator::I64AtomicRmwCmpxchg { memarg }
        | Operator::I32AtomicRmw8CmpxchgU { memarg }
        | Operator::I32AtomicRmw16CmpxchgU { memarg }
        | Operator::I64AtomicRmw8CmpxchgU { memarg }
        | Operator::I64AtomicRmw16CmpxchgU { memarg }
        | Operator::I64AtomicRmw32CmpxchgU { memarg }
        | Operator::V128Load { memarg }
        | Operator::V128Load8x8S { memarg }
        | Operator::V128Load8x8U { memarg }
        | Operator::V128Load16x4S { memarg }
        | Operator::V128Load16x4U { memarg }
        | Operator::V128Load32x2S { memarg }
        | Operator::V128Load32x2U { memarg }
        | Operator::V128Load8Splat { memarg }
        | Operator::V128Load16Splat { memarg }
        | Operator::V128Load32Splat { memarg }
        | Operator::V128Load64Splat { memarg }
        | Operator::V128Load32Zero { memarg }
        | Operator::V128Load64Zero { memarg }
        | Operator::V128Store { memarg }
        | Operator::V128Load8Lane { memarg, .. }
        | Operator::V128Load16Lane { memarg, .. }
        | Operator::V128Load32Lane { memarg, .. }
        | Operator::V128Load64Lane { memarg, .. }
        | Operator::V128Store8Lane { memarg, .. }
        | Operator::V128Store16Lane { memarg, .. }
        | Operator::V128Store32Lane { memarg, .. }
        | Operator::V128Store64Lane { memarg, .. } => {
            record(
                function,
                offset,
                Dependency::Memory(memarg.memory),
                "memory_access",
            );
        }
        Operator::MemorySize { mem } => {
            record(function, offset, Dependency::Memory(*mem), "memory_size");
        }
        Operator::MemoryGrow { mem } => {
            record(function, offset, Dependency::Memory(*mem), "memory_grow");
        }
        Operator::TableGet { table } => {
            record(function, offset, Dependency::Table(*table), "table_get");
        }
        Operator::TableSet { table } => {
            record(function, offset, Dependency::Table(*table), "table_set");
        }
        Operator::TableSize { table } => {
            record(function, offset, Dependency::Table(*table), "table_size");
        }
        Operator::TableGrow { table } => {
            record(function, offset, Dependency::Table(*table), "table_grow");
        }
        Operator::TableFill { table } => {
            record(function, offset, Dependency::Table(*table), "table_fill");
        }
        Operator::TableInit { elem_index, table } => {
            record(
                function,
                offset,
                Dependency::Element(*elem_index),
                "table_init",
            );
            record(function, offset, Dependency::Table(*table), "table_init");
        }
        Operator::TableCopy {
            dst_table,
            src_table,
        } => {
            record(
                function,
                offset,
                Dependency::Table(*dst_table),
                "table_copy",
            );
            record(
                function,
                offset,
                Dependency::Table(*src_table),
                "table_copy",
            );
        }
        Operator::MemoryInit { data_index, mem } => {
            record(
                function,
                offset,
                Dependency::Data(*data_index),
                "memory_init",
            );
            record(function, offset, Dependency::Memory(*mem), "memory_init");
        }
        Operator::DataDrop { data_index } => {
            record(function, offset, Dependency::Data(*data_index), "data_drop");
        }
        Operator::MemoryCopy { dst_mem, src_mem } => {
            record(
                function,
                offset,
                Dependency::Memory(*dst_mem),
                "memory_copy",
            );
            record(
                function,
                offset,
                Dependency::Memory(*src_mem),
                "memory_copy",
            );
        }
        Operator::MemoryFill { mem } => {
            record(function, offset, Dependency::Memory(*mem), "memory_fill");
        }
        Operator::MemoryDiscard { mem } => {
            record(function, offset, Dependency::Memory(*mem), "memory_discard");
        }
        Operator::TryTable { try_table } => {
            for catch in &try_table.catches {
                match catch {
                    wasmparser::Catch::One { tag, .. } | wasmparser::Catch::OneRef { tag, .. } => {
                        record(function, offset, Dependency::Tag(*tag), "try_table");
                    }
                    wasmparser::Catch::All { .. } | wasmparser::Catch::AllRef { .. } => {}
                }
            }
        }
        Operator::ElemDrop { elem_index } => {
            record(
                function,
                offset,
                Dependency::Element(*elem_index),
                "elem_drop",
            );
        }
        Operator::Throw { tag_index } => {
            record(function, offset, Dependency::Tag(*tag_index), "throw");
        }
        Operator::Catch { tag_index } => {
            record(function, offset, Dependency::Tag(*tag_index), "catch");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze, Dependency, FunctionImport, GraphError, RouteSplitRoot, ValidationError};

    #[test]
    fn ownership_plan_keeps_indirect_route_funcs_off_shell() {
        // Mirrors the HN bug: a route reaches a function only via the funcref
        // table, and that function's mangled name says it belongs to the route.
        // The legacy planner pulled it into shell ("used by all" via Table(0)
        // expansion); the ownership-aware planner should leave it in the route.
        //
        // WAT identifiers can't carry the `::routes::a::` substring the
        // classifier looks for, so build the module with wasm_encoder and
        // attach a name section by hand.
        let wasm = build_test_module_with_names(&[
            (0, "route_a_entry"),
            (1, "route_b_entry"),
            (2, "shell_helper"),
            (3, "<crate::routes::a::Foo>::handler"),
        ]);

        let module = analyze(&wasm).unwrap();
        let target = module.functions.iter().find(|f| f.index == 3).unwrap();
        assert_eq!(
            target.name.as_deref(),
            Some("<crate::routes::a::Foo>::handler")
        );

        let routes = vec![
            RouteSplitRoot {
                name: "a".to_string(),
                roots: vec![Dependency::Function(0)],
            },
            RouteSplitRoot {
                name: "b".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let route_ids = vec!["a".to_string(), "b".to_string()];
        let (plan, _report) = module
            .plan_route_split_with_ownership([Dependency::Function(2)], &routes, &route_ids)
            .unwrap();

        let target_dep = Dependency::Function(3);
        assert!(
            !plan.shell.contains(&target_dep),
            "ownership planner should not pin route-named indirect targets to shell"
        );
        assert!(
            plan.routes[0].dependencies.contains(&target_dep),
            "indirect target with `routes::a::` in its name should belong to route a"
        );
        assert!(
            !plan.routes[1].dependencies.contains(&target_dep),
            "route b should not own a function whose name says route a"
        );
    }

    fn build_test_module_with_names(names: &[(u32, &str)]) -> Vec<u8> {
        // Module with: 2 types (void, entry-with-i32-param), one funcref table,
        // 4 funcs (3 entries that call_indirect, 1 indirect-only target),
        // an active elem segment installing the indirect target, and exports
        // for the shell + route roots.
        use wasm_encoder::*;
        let mut module = Module::new();

        let mut types = TypeSection::new();
        types.ty().function([], []); // type 0: () -> ()
        types.ty().function([ValType::I32], []); // type 1: (i32) -> ()
        module.section(&types);

        let mut funcs = FunctionSection::new();
        funcs.function(1); // route_a_entry: (i32) -> ()
        funcs.function(1); // route_b_entry: (i32) -> ()
        funcs.function(0); // shell_helper: () -> ()
        funcs.function(0); // a_only_indirect: () -> ()
        module.section(&funcs);

        let mut tables = TableSection::new();
        tables.table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: 1,
            maximum: Some(1),
            shared: false,
        });
        module.section(&tables);

        let mut exports = ExportSection::new();
        exports.export("pocopine_route_mount_a", ExportKind::Func, 0);
        exports.export("pocopine_route_mount_b", ExportKind::Func, 1);
        exports.export("shell_root", ExportKind::Func, 2);
        module.section(&exports);

        let mut elements = ElementSection::new();
        elements.active(
            None,
            &ConstExpr::i32_const(0),
            Elements::Functions(std::borrow::Cow::Owned(vec![3])),
        );
        module.section(&elements);

        let mut codes = CodeSection::new();
        // route_a_entry / route_b_entry: local.get 0; call_indirect type 0
        for _ in 0..2 {
            let mut f = Function::new([]);
            f.instruction(&Instruction::LocalGet(0));
            f.instruction(&Instruction::CallIndirect {
                type_index: 0,
                table_index: 0,
            });
            f.instruction(&Instruction::End);
            codes.function(&f);
        }
        // shell_helper + a_only_indirect: empty bodies
        for _ in 0..2 {
            let mut f = Function::new([]);
            f.instruction(&Instruction::End);
            codes.function(&f);
        }
        module.section(&codes);

        // Encode the name custom section by hand.
        let mut name_payload = Vec::new();
        // Subsection 1: function names; vec(naming) where naming = (idx, name)
        let mut subsection = Vec::new();
        // count
        encode_u32(&mut subsection, names.len() as u32);
        for (idx, name) in names {
            encode_u32(&mut subsection, *idx);
            encode_u32(&mut subsection, name.len() as u32);
            subsection.extend_from_slice(name.as_bytes());
        }
        name_payload.push(0x01); // subsection id: function names
        encode_u32(&mut name_payload, subsection.len() as u32);
        name_payload.extend_from_slice(&subsection);
        module.section(&CustomSection {
            name: std::borrow::Cow::Borrowed("name"),
            data: std::borrow::Cow::Owned(name_payload),
        });

        module.finish()
    }

    fn encode_u32(buf: &mut Vec<u8>, mut v: u32) {
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if v == 0 {
                break;
            }
        }
    }

    #[test]
    fn classify_function_owner_picks_route_or_shared() {
        use super::FunctionOwner;
        let routes = vec!["home".to_string(), "story".to_string()];

        let shell =
            FunctionOwner::from_name(Some("<core::ptr::drop_in_place::<Box<dyn Fn()>>>"), &routes);
        assert_eq!(shell, FunctionOwner::Shell);

        let home = FunctionOwner::from_name(
            Some("<hn[abc123]::routes::home::StoryList>::__pocopine_mount_template"),
            &routes,
        );
        assert_eq!(home, FunctionOwner::Route("home".to_string()));

        let shared = FunctionOwner::from_name(
            Some("pocopine_core[xyz]::stamp_if_body_with::<hn::routes::home::A, hn::routes::story::B>"),
            &routes,
        );
        match shared {
            FunctionOwner::Shared(routes) => {
                assert!(routes.contains(&"home".to_string()));
                assert!(routes.contains(&"story".to_string()));
            }
            other => panic!("expected Shared, got {other:?}"),
        }

        let unknown = FunctionOwner::from_name(None, &routes);
        assert_eq!(unknown, FunctionOwner::Unknown);
    }

    #[test]
    fn analyze_extracts_function_names_from_name_section() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (func $alpha (type $t0))
              (func $beta (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let alpha = module.functions.iter().find(|f| f.index == 0).unwrap();
        let beta = module.functions.iter().find(|f| f.index == 1).unwrap();

        assert_eq!(alpha.name.as_deref(), Some("alpha"));
        assert_eq!(beta.name.as_deref(), Some("beta"));
    }

    #[test]
    fn analyze_handles_modules_without_name_section() {
        // Strip a name section to confirm we don't choke without one.
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (func (type 0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        // wat-generated modules sometimes embed names; guard either outcome.
        let func = module.functions.iter().find(|f| f.defined).unwrap();
        assert!(func.name.is_none() || func.name.as_deref() == Some(""));
    }

    #[test]
    fn scans_function_indices_without_relocations() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (func $helper (type $t0))
              (func $stub (type $t0)
                call $helper)
              (export "stub" (func $stub)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let stub = module.functions.iter().find(|f| f.index == 1).unwrap();

        assert!(stub.dependencies.contains(&Dependency::Function(0)));
        assert_eq!(stub.index_uses[0].operator, "call");
    }

    #[test]
    fn scans_non_function_index_uses() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (global $g (mut i32) (i32.const 0))
              (table $table 1 funcref)
              (elem declare func $f)
              (func $f (type $t0)
                global.get $g
                drop
                ref.func $f
                drop
                i32.const 0
                table.get $table
                drop))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let function = module.functions.iter().find(|f| f.defined).unwrap();

        assert!(function.dependencies.contains(&Dependency::Global(0)));
        assert!(function.dependencies.contains(&Dependency::Function(0)));
        assert!(function.dependencies.contains(&Dependency::Table(0)));

        assert_eq!(module.index_spaces.functions, 1);
        assert_eq!(module.index_spaces.globals, 1);
        assert_eq!(module.index_spaces.tables, 1);
        assert_eq!(module.index_spaces.elements, 1);
        module.validate_indices().unwrap();
    }

    #[test]
    fn validation_rejects_out_of_bounds_instruction_indices() {
        let mut module = wasm_encoder::Module::new();
        let mut types = wasm_encoder::TypeSection::new();
        types.ty().function([], []);
        module.section(&types);

        let mut functions = wasm_encoder::FunctionSection::new();
        functions.function(0);
        module.section(&functions);

        let mut code = wasm_encoder::CodeSection::new();
        let mut function = wasm_encoder::Function::new([]);
        function.instruction(&wasm_encoder::Instruction::Call(99));
        function.instruction(&wasm_encoder::Instruction::End);
        code.function(&function);
        module.section(&code);

        let analysis = analyze(&module.finish()).unwrap();
        let errors = analysis.validate_indices().unwrap_err();

        assert!(matches!(
            errors.as_slice(),
            [ValidationError::FunctionIndexOutOfBounds {
                function: 0,
                operator: "call",
                dependency: Dependency::Function(99),
                limit: 1,
                ..
            }]
        ));
    }

    #[test]
    fn validation_rejects_out_of_bounds_function_type_indices() {
        let mut module = wasm_encoder::Module::new();
        let mut types = wasm_encoder::TypeSection::new();
        types.ty().function([], []);
        module.section(&types);

        let mut functions = wasm_encoder::FunctionSection::new();
        functions.function(7);
        module.section(&functions);

        let mut code = wasm_encoder::CodeSection::new();
        let mut function = wasm_encoder::Function::new([]);
        function.instruction(&wasm_encoder::Instruction::End);
        code.function(&function);
        module.section(&code);

        let analysis = analyze(&module.finish()).unwrap();
        let errors = analysis.validate_indices().unwrap_err();

        assert!(matches!(
            errors.as_slice(),
            [ValidationError::FunctionTypeIndexOutOfBounds {
                function: 0,
                type_index: 7,
                limit: 1,
            }]
        ));
    }

    #[test]
    fn dependency_closure_walks_transitive_function_edges() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (global $g i32 (i32.const 7))
              (func $leaf (type $t0)
                global.get $g
                drop)
              (func $middle (type $t0)
                call $leaf)
              (func $root (type $t0)
                call $middle))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let closure = module
            .dependency_closure([Dependency::Function(2)])
            .unwrap();

        assert!(closure.contains(&Dependency::Function(2)));
        assert!(closure.contains(&Dependency::Function(1)));
        assert!(closure.contains(&Dependency::Function(0)));
        assert!(closure.contains(&Dependency::Type(0)));
        assert!(closure.contains(&Dependency::Global(0)));
    }

    #[test]
    fn dependency_closure_rejects_out_of_bounds_roots() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (func $root (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let error = module
            .dependency_closure([Dependency::Function(9)])
            .unwrap_err();

        assert_eq!(
            error,
            GraphError::RootIndexOutOfBounds {
                dependency: Dependency::Function(9),
                limit: 1,
            }
        );
    }

    #[test]
    fn route_split_plan_classifies_shell_route_and_shared_dependencies() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (func $shell_util (type $t0))
              (func $route_a_leaf (type $t0))
              (func $shared_ab_helper (type $t0))
              (func $route_a_entry (type $t0)
                call $route_a_leaf
                call $shared_ab_helper)
              (func $route_b_entry (type $t0)
                call $shared_ab_helper)
              (func $route_c_entry (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "a".to_string(),
                roots: vec![Dependency::Function(3)],
            },
            RouteSplitRoot {
                name: "b".to_string(),
                roots: vec![Dependency::Function(4)],
            },
            RouteSplitRoot {
                name: "c".to_string(),
                roots: vec![Dependency::Function(5)],
            },
        ];

        let plan = module
            .plan_route_split([Dependency::Function(0)], &routes)
            .unwrap();

        assert!(plan.shell.contains(&Dependency::Function(0)));
        assert!(plan.shell.contains(&Dependency::Type(0)));
        assert!(plan.routes[0]
            .dependencies
            .contains(&Dependency::Function(3)));
        assert!(plan.routes[0]
            .dependencies
            .contains(&Dependency::Function(1)));
        assert!(plan.routes[1]
            .dependencies
            .contains(&Dependency::Function(4)));
        assert!(plan.routes[2]
            .dependencies
            .contains(&Dependency::Function(5)));

        assert_eq!(plan.shared.len(), 1);
        assert_eq!(plan.shared[0].routes, vec!["a", "b"]);
        assert!(plan.shared[0]
            .dependencies
            .contains(&Dependency::Function(2)));
        assert!(!plan.routes[0]
            .dependencies
            .contains(&Dependency::Function(2)));
        assert!(!plan.routes[1]
            .dependencies
            .contains(&Dependency::Function(2)));
    }

    #[test]
    fn route_split_plan_promotes_all_route_dependencies_to_shell() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (func $shared_all_helper (type $t0))
              (func $route_a_entry (type $t0)
                call $shared_all_helper)
              (func $route_b_entry (type $t0)
                call $shared_all_helper))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "a".to_string(),
                roots: vec![Dependency::Function(1)],
            },
            RouteSplitRoot {
                name: "b".to_string(),
                roots: vec![Dependency::Function(2)],
            },
        ];

        let plan = module.plan_route_split([], &routes).unwrap();

        assert!(plan.shell.contains(&Dependency::Function(0)));
        assert!(plan.shell.contains(&Dependency::Type(0)));
        assert!(plan.routes[0]
            .dependencies
            .contains(&Dependency::Function(1)));
        assert!(plan.routes[1]
            .dependencies
            .contains(&Dependency::Function(2)));
        assert!(plan.shared.is_empty());
    }

    #[test]
    fn split_plan_builds_compact_remap_tables() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (func $shell_util (type $t0))
              (func $route_a_leaf (type $t0))
              (func $shared_ab_helper (type $t0))
              (func $route_a_entry (type $t0)
                call $route_a_leaf
                call $shared_ab_helper)
              (func $route_b_entry (type $t0)
                call $shared_ab_helper)
              (func $route_c_entry (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "a".to_string(),
                roots: vec![Dependency::Function(3)],
            },
            RouteSplitRoot {
                name: "b".to_string(),
                roots: vec![Dependency::Function(4)],
            },
            RouteSplitRoot {
                name: "c".to_string(),
                roots: vec![Dependency::Function(5)],
            },
        ];

        let plan = module
            .plan_route_split([Dependency::Function(0)], &routes)
            .unwrap();
        let remaps = plan.build_remaps();

        assert_eq!(remaps.shell.remap.remap(Dependency::Function(0)), Some(0));
        assert_eq!(remaps.shell.remap.remap(Dependency::Type(0)), Some(0));
        assert_eq!(remaps.routes[0].name, "a");
        assert_eq!(
            remaps.routes[0].remap.remap(Dependency::Function(1)),
            Some(0)
        );
        assert_eq!(
            remaps.routes[0].remap.remap(Dependency::Function(3)),
            Some(1)
        );
        assert_eq!(remaps.routes[0].remap.remap(Dependency::Function(2)), None);
        assert_eq!(remaps.routes[1].name, "b");
        assert_eq!(
            remaps.routes[1].remap.remap(Dependency::Function(4)),
            Some(0)
        );
        assert_eq!(remaps.shared[0].name, "shared:a+b");
        assert_eq!(
            remaps.shared[0].remap.remap(Dependency::Function(2)),
            Some(0)
        );
    }

    #[test]
    fn link_plan_separates_owned_dependencies_from_externals() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (import "env" "host" (func $host (type $t0)))
              (func $shared_ab_helper (type $t0)
                call $host)
              (func $route_a_entry (type $t0)
                call $shared_ab_helper)
              (func $route_b_entry (type $t0)
                call $shared_ab_helper)
              (func $route_c_entry (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        assert!(module.imports.contains(&Dependency::Function(0)));

        let routes = vec![
            RouteSplitRoot {
                name: "a".to_string(),
                roots: vec![Dependency::Function(2)],
            },
            RouteSplitRoot {
                name: "b".to_string(),
                roots: vec![Dependency::Function(3)],
            },
            RouteSplitRoot {
                name: "c".to_string(),
                roots: vec![Dependency::Function(4)],
            },
        ];

        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);
        module.validate_link_plan(&links).unwrap();

        assert!(links.shell.owned.contains(&Dependency::Type(0)));
        assert!(links.routes[0].owned.contains(&Dependency::Function(2)));
        assert!(links.routes[0].external.contains(&Dependency::Function(1)));
        assert!(links.routes[0].external.contains(&Dependency::Type(0)));
        assert_eq!(
            links.routes[0]
                .external_remap
                .remap(Dependency::Function(1)),
            Some(0)
        );

        assert!(links.shared[0].owned.contains(&Dependency::Function(1)));
        assert!(links.shared[0].external.contains(&Dependency::Function(0)));
        assert!(links.shared[0].external.contains(&Dependency::Type(0)));
        assert_eq!(
            links.shared[0]
                .external_remap
                .remap(Dependency::Function(0)),
            Some(0)
        );
    }

    #[test]
    fn link_validation_rejects_unresolved_instruction_indices() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (func $shell_helper (type $t0))
              (func $route_entry (type $t0)
                call $shell_helper)
              (func $other_route_entry (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(1)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(2)],
            },
        ];
        let plan = module
            .plan_route_split([Dependency::Function(0)], &routes)
            .unwrap();
        let mut links = module.build_link_plan(&plan);

        links.routes[0].external_remap = super::IndexRemap::default();
        let errors = module.validate_link_plan(&links).unwrap_err();

        assert!(errors.iter().any(|error| matches!(
            error,
            super::LinkError::MissingInstructionRemap {
                chunk,
                function: 1,
                operator: "call",
                dependency: Dependency::Function(0),
                ..
            } if chunk == "route"
        )));
    }

    #[test]
    fn emits_function_chunk_with_remapped_calls() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func (result i32)))
              (func $shell (type $t0)
                i32.const 0)
              (func $route_leaf (type $t0)
                i32.const 7)
              (func $route_entry (type $t0)
                call $route_leaf)
              (func $other_route_entry (type $t0)
                i32.const 3))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(2)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(3)],
            },
        ];
        let plan = module
            .plan_route_split([Dependency::Function(0)], &routes)
            .unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let emitted = analyze(&emitted).unwrap();
        assert_eq!(emitted.index_spaces.types, 1);
        assert_eq!(emitted.index_spaces.functions, 2);
        let route_entry = emitted.functions.iter().find(|f| f.index == 1).unwrap();
        assert!(route_entry.dependencies.contains(&Dependency::Function(0)));
    }

    #[test]
    fn emits_original_function_import_names() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func (result i32)))
              (import "env" "host_value" (func $host_value (type $t0)))
              (func $route_entry (type $t0)
                call $host_value)
              (func $other_route_entry (type $t0)
                i32.const 3))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        assert_eq!(
            module.function_imports.get(&0).unwrap(),
            &FunctionImport {
                function: 0,
                module: "env".to_string(),
                name: "host_value".to_string(),
                type_index: 0,
            }
        );

        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(1)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(2)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let imports = function_import_names(&emitted);
        assert_eq!(
            imports,
            vec![("env".to_string(), "host_value".to_string(), 0)]
        );
    }

    #[test]
    fn emits_only_owned_function_exports() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func (result i32)))
              (func $shell (type $t0)
                i32.const 0)
              (func $route_entry (type $t0)
                call $shell)
              (func $other_route_entry (type $t0)
                i32.const 3)
              (export "shell" (func $shell))
              (export "route_entry" (func $route_entry)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(1)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(2)],
            },
        ];
        let plan = module
            .plan_route_split([Dependency::Function(0)], &routes)
            .unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        assert_eq!(function_export_names(&emitted), vec!["route_entry"]);
    }

    #[test]
    fn emits_owned_non_function_exports() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (table $table 1 funcref)
              (memory $memory 1)
              (global $global (mut i32) (i32.const 0))
              (func $shell (type $t0)
                global.get $global
                drop)
              (export "shell" (func $shell))
              (export "memory" (memory $memory))
              (export "__wbindgen_externrefs" (table $table))
              (export "__stack_pointer" (global $global)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let plan = module
            .plan_route_split(
                [
                    Dependency::Function(0),
                    Dependency::Memory(0),
                    Dependency::Table(0),
                ],
                &[],
            )
            .unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.shell).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let exports = export_names(&emitted);
        assert!(exports.contains(&"shell".to_string()));
        assert!(exports.contains(&"memory".to_string()));
        assert!(exports.contains(&"__wbindgen_externrefs".to_string()));
        assert!(exports.contains(&"__stack_pointer".to_string()));
    }

    #[test]
    fn emits_defined_table_for_indirect_calls() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func (result i32)))
              (table $table 1 funcref)
              (func $route_entry (type $t0)
                i32.const 0
                call_indirect (type $t0))
              (func $other_route_entry (type $t0)
                i32.const 3))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let emitted = analyze(&emitted).unwrap();
        assert_eq!(emitted.index_spaces.tables, 1);
        let route_entry = emitted.functions.iter().find(|f| f.index == 0).unwrap();
        assert!(route_entry.dependencies.contains(&Dependency::Table(0)));
    }

    #[test]
    fn table_dependency_closure_includes_active_element_segment() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (table $table 1 funcref)
              (func $target (type $t0))
              (elem (i32.const 0) $target))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let closure = module.dependency_closure([Dependency::Table(0)]).unwrap();

        assert!(closure.contains(&Dependency::Table(0)));
        assert!(closure.contains(&Dependency::Element(0)));
        assert!(closure.contains(&Dependency::Function(0)));
    }

    #[test]
    fn emits_original_table_import_names() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func (result i32)))
              (import "env" "indirect_table" (table $table 1 funcref))
              (func $route_entry (type $t0)
                i32.const 0
                call_indirect (type $t0))
              (func $other_route_entry (type $t0)
                i32.const 3))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        assert_eq!(module.table_imports.get(&0).unwrap().module, "env");
        assert_eq!(module.table_imports.get(&0).unwrap().name, "indirect_table");

        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        assert_eq!(
            table_import_names(&emitted),
            vec![("env".to_string(), "indirect_table".to_string())]
        );
    }

    #[test]
    fn memory_dependency_closure_includes_active_data_segment() {
        let wasm = wat::parse_str(
            r#"
            (module
              (memory $memory 1)
              (data (i32.const 8) "hello"))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let closure = module.dependency_closure([Dependency::Memory(0)]).unwrap();

        assert!(closure.contains(&Dependency::Memory(0)));
        assert!(closure.contains(&Dependency::Data(0)));
    }

    #[test]
    fn emits_active_function_element_segments() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (table $table 1 funcref)
              (func $route_entry (type $t0))
              (func $other_route_entry (type $t0))
              (elem (i32.const 0) $route_entry))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        assert_eq!(module.elements.len(), 1);
        assert!(module.elements[0]
            .dependencies
            .contains(&Dependency::Function(0)));
        assert!(module.elements[0]
            .dependencies
            .contains(&Dependency::Table(0)));

        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0), Dependency::Element(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let emitted = analyze(&emitted).unwrap();
        assert_eq!(emitted.index_spaces.elements, 1);
        assert_eq!(emitted.index_spaces.tables, 1);
        assert!(emitted.elements[0]
            .dependencies
            .contains(&Dependency::Function(0)));
        assert!(emitted.elements[0]
            .dependencies
            .contains(&Dependency::Table(0)));
    }

    #[test]
    fn emits_passive_element_segments_for_table_init() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (table $table 1 funcref)
              (func $route_leaf (type $t0))
              (elem $segment func $route_leaf)
              (func $route_entry (type $t0)
                i32.const 0
                i32.const 0
                i32.const 1
                table.init $table $segment)
              (func $other_route_entry (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(1)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(2)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let emitted = analyze(&emitted).unwrap();
        assert_eq!(emitted.index_spaces.elements, 1);
        assert!(emitted.functions[1]
            .dependencies
            .contains(&Dependency::Element(0)));
    }

    #[test]
    fn emits_defined_globals_and_remaps_global_get() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func (result i32)))
              (global $g (mut i32) (i32.const 7))
              (func $route_entry (type $t0)
                global.get $g)
              (func $other_route_entry (type $t0)
                i32.const 3))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let emitted = analyze(&emitted).unwrap();
        assert_eq!(emitted.index_spaces.globals, 1);
        assert!(emitted.functions[0]
            .dependencies
            .contains(&Dependency::Global(0)));
    }

    #[test]
    fn emits_original_global_import_names() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func (result i32)))
              (import "env" "host_global" (global $host_global i32))
              (func $route_entry (type $t0)
                global.get $host_global)
              (func $other_route_entry (type $t0)
                i32.const 3))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        assert_eq!(module.global_imports.get(&0).unwrap().module, "env");
        assert_eq!(module.global_imports.get(&0).unwrap().name, "host_global");

        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        assert_eq!(
            global_import_names(&emitted),
            vec![("env".to_string(), "host_global".to_string())]
        );
    }

    #[test]
    fn emits_defined_memory_and_active_data_segments() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func (result i32)))
              (memory $mem 1)
              (data (i32.const 0) "route-data")
              (func $route_entry (type $t0)
                i32.const 0
                i32.load)
              (func $other_route_entry (type $t0)
                i32.const 3))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        assert_eq!(module.memories.len(), 1);
        assert_eq!(module.data_segments.len(), 1);
        assert!(module.data_segments[0]
            .dependencies
            .contains(&Dependency::Memory(0)));

        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0), Dependency::Data(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let emitted = analyze(&emitted).unwrap();
        assert_eq!(emitted.index_spaces.memories, 1);
        assert_eq!(emitted.index_spaces.data, 1);
        assert!(emitted.functions[0]
            .dependencies
            .contains(&Dependency::Memory(0)));
        assert!(emitted.data_segments[0]
            .dependencies
            .contains(&Dependency::Memory(0)));
    }

    #[test]
    fn emits_passive_data_segments_for_memory_init() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (memory $mem 1)
              (data $segment "abc")
              (func $route_entry (type $t0)
                i32.const 0
                i32.const 0
                i32.const 3
                memory.init $segment)
              (func $other_route_entry (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let emitted = analyze(&emitted).unwrap();
        assert_eq!(emitted.index_spaces.memories, 1);
        assert_eq!(emitted.index_spaces.data, 1);
        assert!(emitted.functions[0]
            .dependencies
            .contains(&Dependency::Data(0)));
        assert!(emitted.functions[0]
            .dependencies
            .contains(&Dependency::Memory(0)));
    }

    #[test]
    fn emits_original_memory_import_names() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func (result i32)))
              (import "env" "memory" (memory $mem 1))
              (data (i32.const 0) "route-data")
              (func $route_entry (type $t0)
                i32.const 0
                i32.load)
              (func $other_route_entry (type $t0)
                i32.const 3))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        assert_eq!(module.memory_imports.get(&0).unwrap().module, "env");
        assert_eq!(module.memory_imports.get(&0).unwrap().name, "memory");

        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0), Dependency::Data(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        assert_eq!(
            memory_import_names(&emitted),
            vec![("env".to_string(), "memory".to_string())]
        );
    }

    #[test]
    fn emits_defined_tags_and_remaps_throw() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (tag $route_error (type $t0))
              (func $route_entry (type $t0)
                throw $route_error)
              (func $other_route_entry (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        assert_eq!(module.tags.len(), 1);
        assert!(module.tags[0].dependencies.contains(&Dependency::Type(0)));

        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        let emitted = analyze(&emitted).unwrap();
        assert_eq!(emitted.index_spaces.tags, 1);
        assert!(emitted.functions[0]
            .dependencies
            .contains(&Dependency::Tag(0)));
    }

    #[test]
    fn emits_original_tag_import_names() {
        let wasm = wat::parse_str(
            r#"
            (module
              (type $t0 (func))
              (import "env" "route_error" (tag $route_error (type $t0)))
              (func $route_entry (type $t0)
                throw $route_error)
              (func $other_route_entry (type $t0)))
            "#,
        )
        .unwrap();

        let module = analyze(&wasm).unwrap();
        assert_eq!(module.tag_imports.get(&0).unwrap().module, "env");
        assert_eq!(module.tag_imports.get(&0).unwrap().name, "route_error");

        let routes = vec![
            RouteSplitRoot {
                name: "route".to_string(),
                roots: vec![Dependency::Function(0)],
            },
            RouteSplitRoot {
                name: "other".to_string(),
                roots: vec![Dependency::Function(1)],
            },
        ];
        let plan = module.plan_route_split([], &routes).unwrap();
        let links = module.build_link_plan(&plan);

        let emitted = module.emit_function_chunk(&links.routes[0]).unwrap();
        wasmparser::Validator::new().validate_all(&emitted).unwrap();

        assert_eq!(
            tag_import_names(&emitted),
            vec![("env".to_string(), "route_error".to_string())]
        );
    }

    fn function_import_names(wasm: &[u8]) -> Vec<(String, String, u32)> {
        let mut imports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(wasm) {
            if let wasmparser::Payload::ImportSection(reader) = payload.unwrap() {
                for import in reader {
                    let import = import.unwrap();
                    if let wasmparser::TypeRef::Func(type_index) = import.ty {
                        imports.push((
                            import.module.to_string(),
                            import.name.to_string(),
                            type_index,
                        ));
                    }
                }
            }
        }
        imports
    }

    fn function_export_names(wasm: &[u8]) -> Vec<String> {
        let mut exports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(wasm) {
            if let wasmparser::Payload::ExportSection(reader) = payload.unwrap() {
                for export in reader {
                    let export = export.unwrap();
                    if export.kind == wasmparser::ExternalKind::Func {
                        exports.push(export.name.to_string());
                    }
                }
            }
        }
        exports
    }

    fn export_names(wasm: &[u8]) -> Vec<String> {
        let mut exports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(wasm) {
            if let wasmparser::Payload::ExportSection(reader) = payload.unwrap() {
                for export in reader {
                    exports.push(export.unwrap().name.to_string());
                }
            }
        }
        exports
    }

    fn table_import_names(wasm: &[u8]) -> Vec<(String, String)> {
        let mut imports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(wasm) {
            if let wasmparser::Payload::ImportSection(reader) = payload.unwrap() {
                for import in reader {
                    let import = import.unwrap();
                    if matches!(import.ty, wasmparser::TypeRef::Table(_)) {
                        imports.push((import.module.to_string(), import.name.to_string()));
                    }
                }
            }
        }
        imports
    }

    fn memory_import_names(wasm: &[u8]) -> Vec<(String, String)> {
        let mut imports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(wasm) {
            if let wasmparser::Payload::ImportSection(reader) = payload.unwrap() {
                for import in reader {
                    let import = import.unwrap();
                    if matches!(import.ty, wasmparser::TypeRef::Memory(_)) {
                        imports.push((import.module.to_string(), import.name.to_string()));
                    }
                }
            }
        }
        imports
    }

    fn tag_import_names(wasm: &[u8]) -> Vec<(String, String)> {
        let mut imports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(wasm) {
            if let wasmparser::Payload::ImportSection(reader) = payload.unwrap() {
                for import in reader {
                    let import = import.unwrap();
                    if matches!(import.ty, wasmparser::TypeRef::Tag(_)) {
                        imports.push((import.module.to_string(), import.name.to_string()));
                    }
                }
            }
        }
        imports
    }

    fn global_import_names(wasm: &[u8]) -> Vec<(String, String)> {
        let mut imports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(wasm) {
            if let wasmparser::Payload::ImportSection(reader) = payload.unwrap() {
                for import in reader {
                    let import = import.unwrap();
                    if matches!(import.ty, wasmparser::TypeRef::Global(_)) {
                        imports.push((import.module.to_string(), import.name.to_string()));
                    }
                }
            }
        }
        imports
    }
}
