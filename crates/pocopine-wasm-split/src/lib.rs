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
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModuleAnalysis {
    pub imported_functions: u32,
    pub index_spaces: IndexSpaces,
    pub imports: BTreeSet<Dependency>,
    pub function_imports: BTreeMap<u32, FunctionImport>,
    pub table_imports: BTreeMap<u32, TableImport>,
    pub types: Vec<wasmparser::FuncType>,
    pub tables: Vec<TableAnalysis>,
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
pub struct TableAnalysis {
    pub index: u32,
    pub ty: wasmparser::TableType,
    pub imported: bool,
    pub has_init_expr: bool,
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

        let mut exports = wasm_encoder::ExportSection::new();
        for export in &self.exports {
            if export.kind == ExternalKind::Func
                && chunk.local_remap.functions.contains_key(&export.index)
            {
                let index = combined_function_index(chunk, export.index).ok_or_else(|| {
                    EmitError::MissingRemap {
                        chunk: chunk.name.clone(),
                        dependency: Dependency::Function(export.index),
                    }
                })?;
                exports.export(&export.name, wasm_encoder::ExportKind::Func, index);
            }
        }
        if !exports.is_empty() {
            module.section(&exports);
        }

        let mut elements = wasm_encoder::ElementSection::new();
        for old_element in ordered_indices(&chunk.local_remap.elements) {
            self.encode_element(chunk, &mut elements, old_element)?;
        }
        if !elements.is_empty() {
            module.section(&elements);
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

        Ok(module.finish())
    }

    fn validate_chunk_for_emission(&self, chunk: &ChunkLinkPlan) -> Result<(), EmitError> {
        if let Err(errors) = self.validate_single_chunk_link(chunk) {
            return Err(EmitError::Link(errors));
        }

        for dependency in &chunk.owned {
            match dependency {
                Dependency::Function(_)
                | Dependency::Type(_)
                | Dependency::Table(_)
                | Dependency::Element(_) => {}
                _ => {
                    return Err(EmitError::UnsupportedImport {
                        dependency: *dependency,
                    });
                }
            }
        }
        for dependency in &chunk.external {
            match dependency {
                Dependency::Function(_) | Dependency::Type(_) | Dependency::Table(_) => {}
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
                let offset = encode_const_expr(offset_expr);
                elements.active(remapped_table, &offset, items);
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
        Dependency::Element(index) => combined_element_index(chunk, index).ok_or_else(|| {
            ReencodeError::UserError(EmitError::MissingRemap {
                chunk: chunk.name.clone(),
                dependency,
            })
        }),
        Dependency::Memory(_)
        | Dependency::Global(_)
        | Dependency::Tag(_)
        | Dependency::Data(_) => Err(ReencodeError::UserError(EmitError::UnsupportedImport {
            dependency,
        })),
    }
}

fn encode_table_type(ty: wasmparser::TableType) -> Result<wasm_encoder::TableType, EmitError> {
    wasm_encoder::TableType::try_from(ty).map_err(|err| EmitError::Parse(err.to_string()))
}

fn encode_const_expr(expr: &ConstExprAnalysis) -> wasm_encoder::ConstExpr {
    match expr {
        ConstExprAnalysis::I32Const(value) => wasm_encoder::ConstExpr::i32_const(*value),
        ConstExprAnalysis::Unsupported => wasm_encoder::ConstExpr::empty(),
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
                        TypeRef::Memory(_) => {
                            analysis
                                .imports
                                .insert(Dependency::Memory(analysis.index_spaces.memories));
                            analysis.index_spaces.memories += 1;
                        }
                        TypeRef::Global(_) => {
                            analysis
                                .imports
                                .insert(Dependency::Global(analysis.index_spaces.globals));
                            analysis.index_spaces.globals += 1;
                        }
                        TypeRef::Tag(_) => {
                            analysis
                                .imports
                                .insert(Dependency::Tag(analysis.index_spaces.tags));
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
                analysis.index_spaces.memories += count_section(reader)?;
            }
            Payload::GlobalSection(reader) => {
                analysis.index_spaces.globals += count_section(reader)?;
            }
            Payload::TagSection(reader) => {
                analysis.index_spaces.tags += count_section(reader)?;
            }
            Payload::ElementSection(reader) => {
                for element in reader {
                    let index = analysis.index_spaces.elements;
                    analysis.elements.push(analyze_element(index, element?)?);
                    analysis.index_spaces.elements += 1;
                }
            }
            Payload::DataSection(reader) => {
                analysis.index_spaces.data += count_section(reader)?;
            }
            Payload::DataCountSection { count, .. } => {
                analysis.index_spaces.data = analysis.index_spaces.data.max(count);
            }
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

    Ok(analysis)
}

fn count_section<T>(reader: wasmparser::SectionLimited<'_, T>) -> WasmResult<u32> {
    Ok(reader.count())
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
}
