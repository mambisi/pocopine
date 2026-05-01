//! Pocopine-owned wasm splitting analysis.
//!
//! The old prototype splitter treated relocation records as the source of
//! truth and raw-copied function bodies. This crate starts from the opposite
//! invariant: executable code is parsed instruction-by-instruction, and every
//! index-bearing operator is recorded before any future emitter is allowed to
//! split or rewrite a module.

use std::collections::{BTreeMap, BTreeSet};

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
    pub index_uses: Vec<IndexUse>,
    pub dependencies: BTreeSet<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModuleAnalysis {
    pub imported_functions: u32,
    pub index_spaces: IndexSpaces,
    pub functions: Vec<FunctionAnalysis>,
    pub exports: Vec<ExportAnalysis>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    InvalidModule(Vec<ValidationError>),
    RootIndexOutOfBounds { dependency: Dependency, limit: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitPlan {
    pub shell: BTreeSet<Dependency>,
    pub routes: Vec<RouteChunkPlan>,
    pub shared: Vec<SharedChunkPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteChunkPlan {
    pub name: String,
    pub dependencies: BTreeSet<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedChunkPlan {
    pub routes: Vec<String>,
    pub dependencies: BTreeSet<Dependency>,
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

            if let Dependency::Function(index) = dependency {
                let Some(function) = self.function(index) else {
                    return Err(GraphError::RootIndexOutOfBounds {
                        dependency,
                        limit: self.index_spaces.functions,
                    });
                };
                stack.push(Dependency::Type(function.type_index));
                stack.extend(function.dependencies.iter().copied());
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

        for (route_index, route) in routes.iter().enumerate() {
            let closure = self.dependency_closure(route.roots.iter().copied())?;
            for dependency in &closure {
                if !shell.contains(dependency) {
                    usage.entry(*dependency).or_default().insert(route_index);
                }
            }
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
            .map(|(route, dependencies)| RouteChunkPlan {
                name: route.name.clone(),
                dependencies,
            })
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

    fn function(&self, index: u32) -> Option<&FunctionAnalysis> {
        self.functions
            .get(index as usize)
            .filter(|function| function.index == index)
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

pub fn analyze(wasm: &[u8]) -> WasmResult<ModuleAnalysis> {
    let mut analysis = ModuleAnalysis::default();
    let mut imported_function_types = Vec::new();
    let mut defined_function_types = Vec::new();
    let mut defined_function_bodies = Vec::new();

    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::TypeSection(reader) => {
                analysis.index_spaces.types = count_type_section(reader)?;
            }
            Payload::ImportSection(reader) => {
                for import in reader {
                    let import = import?;
                    match import.ty {
                        TypeRef::Func(type_index) => {
                            imported_function_types.push(type_index);
                        }
                        TypeRef::Table(_) => {
                            analysis.index_spaces.tables += 1;
                        }
                        TypeRef::Memory(_) => {
                            analysis.index_spaces.memories += 1;
                        }
                        TypeRef::Global(_) => {
                            analysis.index_spaces.globals += 1;
                        }
                        TypeRef::Tag(_) => {
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
                analysis.index_spaces.tables += count_section(reader)?;
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
                analysis.index_spaces.elements += count_section(reader)?;
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

fn count_type_section(reader: wasmparser::TypeSectionReader<'_>) -> WasmResult<u32> {
    Ok(reader.count())
}

fn count_section<T>(reader: wasmparser::SectionLimited<'_, T>) -> WasmResult<u32> {
    Ok(reader.count())
}

fn empty_defined_function(index: u32, type_index: u32) -> FunctionAnalysis {
    FunctionAnalysis {
        index,
        type_index,
        defined: true,
        index_uses: Vec::new(),
        dependencies: BTreeSet::new(),
    }
}

fn scan_function_body(body: &wasmparser::FunctionBody<'_>) -> WasmResult<FunctionAnalysis> {
    let mut function = FunctionAnalysis {
        index: 0,
        type_index: 0,
        defined: true,
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
    use super::{analyze, Dependency, GraphError, RouteSplitRoot, ValidationError};

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
}
