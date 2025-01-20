use std::iter::once;

use miette::{SourceCode, SourceSpan};
use petgraph::visit::EdgeRef;

use crate::rhdl_core::{
    ast::source::source_location::SourceLocation,
    error::rhdl_error,
    flow_graph::{component::CaseEntry, edge_kind::EdgeKind, error::FlowGraphError},
    hdl::ast::{
        always, assign, binary, bit_string, case, component_instance, concatenate, connection,
        constant, declaration, dynamic_index, dynamic_splice, id, index_bit, initial, port, select,
        unary, unsigned_width, CaseItem, Declaration, Direction, Events, Expression, HDLKind,
        Module, Statement,
    },
    FlowGraph, RHDLError,
};

use super::{
    component::{Component, ComponentKind},
    error::FlowGraphICE,
    flow_graph_impl::{BlackBoxMode, FlowIx},
};

// Generate a register declaration for the given component.

fn node(index: FlowIx) -> String {
    format!("node_{}", index.index())
}

fn nodes(indices: Vec<FlowIx>) -> Vec<Expression> {
    indices.into_iter().map(|ndx| id(&node(ndx))).collect()
}

fn generate_reg_declaration(index: FlowIx, component: &Component) -> Option<Declaration> {
    if component.width == 0 {
        None
    } else {
        let kind = if matches!(component.kind, ComponentKind::BBOutput(_)) {
            HDLKind::Wire
        } else {
            HDLKind::Reg
        };
        Some(declaration(
            kind,
            &node(index),
            unsigned_width(component.width),
            Some(format!("{:?}", component)),
        ))
    }
}

fn arg_fun(index: usize, edge: &EdgeKind) -> Option<usize> {
    match edge {
        EdgeKind::ArgBit(ndx, bit) if *ndx == index => Some(*bit),
        _ => None,
    }
}

struct FlowGraphHDLBuilder<'a> {
    graph: &'a FlowGraph,
    body: Vec<Statement>,
    decls: Vec<Declaration>,
    name: String,
}

impl<'a> FlowGraphHDLBuilder<'a> {
    fn new(name: &'_ str, graph: &'a FlowGraph) -> Self {
        Self {
            graph,
            body: vec![],
            decls: vec![],
            name: name.to_string(),
        }
    }
    fn raise_ice(&self, cause: FlowGraphICE, location: Option<SourceLocation>) -> RHDLError {
        rhdl_error(FlowGraphError {
            cause,
            src: self.graph.code.source(),
            elements: location
                .map(|loc| self.graph.code.span(loc).into())
                .into_iter()
                .collect(),
        })
    }
    // Note that collect_argument is MSB -> LSB
    fn collect_argument<T: Fn(&EdgeKind) -> Option<usize>>(
        &self,
        node: FlowIx,
        width: usize,
        filter: T,
    ) -> Result<Vec<FlowIx>, RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[node];
        let arg_incoming = graph
            .graph
            .edges_directed(node, petgraph::Direction::Incoming)
            .filter_map(|x| filter(x.weight()).map(|ndx| (ndx, x.source())))
            .collect::<Vec<_>>();
        (0..width)
            .map(|bit| {
                let bit = width - 1 - bit;
                arg_incoming
                    .iter()
                    .find_map(|(b, ndx)| if *b == bit { Some(*ndx) } else { None })
                    .ok_or(
                        self.raise_ice(FlowGraphICE::MissingArgument { bit }, component.location),
                    )
            })
            .collect()
    }
    fn stmt(&mut self, statement: Statement) {
        self.body.push(statement);
    }
    fn select_assign_statement(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        let ComponentKind::Select = &component.kind else {
            return Err(self.raise_ice(FlowGraphICE::ExpectedSelectComponent, component.location));
        };
        let control_node = graph
            .graph
            .edges_directed(index, petgraph::Direction::Incoming)
            .find_map(|edge| match edge.weight() {
                EdgeKind::Selector(0) => Some(edge.source()),
                _ => None,
            })
            .ok_or(self.raise_ice(FlowGraphICE::SelectControlNodeNotFound, component.location))?;
        let true_node = graph
            .graph
            .edges_directed(index, petgraph::Direction::Incoming)
            .find_map(|edge| match edge.weight() {
                EdgeKind::True => Some(edge.source()),
                _ => None,
            })
            .ok_or(self.raise_ice(FlowGraphICE::SelectTrueNodeNotFound, component.location))?;
        let false_node = graph
            .graph
            .edges_directed(index, petgraph::Direction::Incoming)
            .find_map(|edge| match edge.weight() {
                EdgeKind::False => Some(edge.source()),
                _ => None,
            })
            .ok_or(self.raise_ice(FlowGraphICE::SelectFalseNodeNotFound, component.location))?;
        self.stmt(assign(
            &node(index),
            select(
                id(&node(control_node)),
                id(&node(true_node)),
                id(&node(false_node)),
            ),
        ));
        Ok(())
    }
    fn dff_input_assign_statement(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        let ComponentKind::BBInput(_) = &component.kind else {
            return Err(self.raise_ice(FlowGraphICE::ExpectedDFFComponent, component.location));
        };
        let data_edge = graph
            .graph
            .edges_directed(index, petgraph::Direction::Incoming)
            .find(|edge| matches!(edge.weight(), EdgeKind::ArgBit(_, _)))
            .ok_or(self.raise_ice(FlowGraphICE::DFFInputDriverNotFound, component.location))?;
        self.stmt(assign(&node(index), id(&node(data_edge.source()))));
        Ok(())
    }
    fn buffer_assign_statement(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        let ComponentKind::Buffer(_name) = &component.kind else {
            return Err(self.raise_ice(FlowGraphICE::ExpectedBufferComponent, component.location));
        };
        // Check for an input buffer case
        if let Some((arg_index, arg_bits)) = graph
            .inputs
            .iter()
            .enumerate()
            .find(|(_, x)| x.contains(&index))
        {
            let bit_pos = arg_bits.iter().position(|x| x == &index).unwrap();
            self.stmt(assign(
                &node(index),
                index_bit(&format!("arg_{}", arg_index), bit_pos),
            ));
        } else {
            let parent = graph
                .graph
                .edges_directed(index, petgraph::Direction::Incoming)
                .next()
                .ok_or(self.raise_ice(FlowGraphICE::BufferParentNotFound, component.location))?;
            self.stmt(assign(&node(index), id(&node(parent.source()))));
        }
        Ok(())
    }

    fn binary_assign_statement(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        let ComponentKind::Binary(bin) = &component.kind else {
            return Err(self.raise_ice(FlowGraphICE::ExpectedBinaryComponent, component.location));
        };
        let arg0 = concatenate(nodes(self.collect_argument(
            index,
            bin.left_len.len(),
            |x| arg_fun(0, x),
        )?));
        let arg0 = if bin.left_len.is_signed() {
            unary(crate::rhdl_core::rtl::spec::AluUnary::Signed, arg0)
        } else {
            unary(crate::rhdl_core::rtl::spec::AluUnary::Unsigned, arg0)
        };
        let arg1 = concatenate(nodes(self.collect_argument(
            index,
            bin.right_len.len(),
            |x| arg_fun(1, x),
        )?));
        let arg1 = if bin.right_len.is_signed() {
            unary(crate::rhdl_core::rtl::spec::AluUnary::Signed, arg1)
        } else {
            unary(crate::rhdl_core::rtl::spec::AluUnary::Unsigned, arg1)
        };
        self.stmt(assign(&node(index), binary(bin.op, arg0, arg1)));
        Ok(())
    }

    fn dynamic_index_assign_statement(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        let ComponentKind::DynamicIndex(dyn_ndx) = &component.kind else {
            return Err(self.raise_ice(FlowGraphICE::ExpectedBinaryComponent, component.location));
        };
        let offset = nodes(
            self.collect_argument(index, dyn_ndx.offset_len, |x| match x {
                EdgeKind::DynamicOffset(ndx) => Some(*ndx),
                _ => None,
            })?,
        );
        let arg = nodes(self.collect_argument(index, dyn_ndx.arg_len, |x| arg_fun(0, x))?);
        // Allocate a new register to hold the argument bits
        let reg_name = format!("dyn_ndx_{}", index.index());
        self.decls.push(declaration(
            HDLKind::Reg,
            &reg_name,
            unsigned_width(dyn_ndx.arg_len),
            Some("Dynamic index temporary register".to_string()),
        ));
        self.stmt(assign(&reg_name, concatenate(arg)));
        self.stmt(assign(
            &node(index),
            dynamic_index(&reg_name, concatenate(offset), component.width),
        ));
        Ok(())
    }

    fn dynamic_splice(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        let ComponentKind::DynamicSplice(splice) = &component.kind else {
            return Err(self.raise_ice(FlowGraphICE::ExpectedBinaryComponent, component.location));
        };
        let lhs_width = component.width;
        let arg = nodes(self.collect_argument(index, lhs_width, |x| arg_fun(0, x))?);
        let arg = concatenate(arg);
        // First copy the argument to the destination register
        self.stmt(assign(&node(index), arg.clone()));
        // Now collect the splice bits (which are the substitution)
        let value = nodes(
            self.collect_argument(index, splice.splice_len, |x| match x {
                EdgeKind::Splice(ndx) => Some(*ndx),
                _ => None,
            })?,
        );
        // Now collect the offset bits
        let offset = nodes(
            self.collect_argument(index, splice.offset_len, |x| match x {
                EdgeKind::DynamicOffset(ndx) => Some(*ndx),
                _ => None,
            })?,
        );
        self.stmt(dynamic_splice(
            &node(index),
            arg,
            concatenate(offset),
            concatenate(value),
            splice.splice_len,
        ));
        Ok(())
    }

    fn unary_assign_statement(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        let ComponentKind::Unary(uny) = &component.kind else {
            return Err(self.raise_ice(FlowGraphICE::ExpectedUnaryComponent, component.location));
        };
        let arg = concatenate(nodes(self.collect_argument(
            index,
            uny.arg_len.len(),
            |x| arg_fun(0, x),
        )?));
        let arg = if uny.arg_len.is_signed() {
            unary(crate::rhdl_core::rtl::spec::AluUnary::Signed, arg)
        } else {
            unary(crate::rhdl_core::rtl::spec::AluUnary::Unsigned, arg)
        };
        self.stmt(assign(&node(index), unary(uny.op, arg)));
        Ok(())
    }

    fn bit_select(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        let ComponentKind::BitSelect(bit_select) = &component.kind else {
            return Err(self.raise_ice(FlowGraphICE::ExpectedUnaryComponent, component.location));
        };
        let arg = self
            .graph
            .graph
            .edges_directed(index, petgraph::Direction::Incoming)
            .next()
            .unwrap();
        let source = arg.source();
        self.stmt(assign(
            &node(index),
            index_bit(&node(source), bit_select.bit_index),
        ));
        Ok(())
    }
    fn case_statement(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        let ComponentKind::Case(kase) = &component.kind else {
            return Err(self.raise_ice(FlowGraphICE::ExpectedCaseComponent, component.location));
        };
        let discriminant =
            nodes(
                self.collect_argument(index, kase.discriminant_width.len(), |x| match x {
                    EdgeKind::Selector(ndx) => Some(*ndx),
                    _ => None,
                })?,
            );
        let lhs = &node(index);
        let table = kase
            .entries
            .iter()
            .map(|entry| match entry {
                CaseEntry::Literal(value) => CaseItem::Literal(value.clone()),
                CaseEntry::WildCard => CaseItem::Wild,
            })
            .enumerate()
            .map(|(arg_ndx, item)| {
                let arg = nodes(
                    self.collect_argument(index, 1, |x| match x {
                        EdgeKind::ArgBit(arg, _) if *arg == arg_ndx => Some(0),
                        _ => None,
                    })?
                    .into_iter()
                    .collect(),
                )[0]
                .clone();
                let statement = assign(lhs, arg);
                Ok((item, statement))
            })
            .collect::<Result<Vec<(CaseItem, Statement)>, RHDLError>>()?;
        let discriminant = concatenate(discriminant);
        self.stmt(case(discriminant, table));
        Ok(())
    }
    fn component(&mut self, index: FlowIx) -> Result<(), RHDLError> {
        let graph = self.graph;
        let component = &graph.graph[index];
        if let Some(location) = component.location {
            let source_text = self.graph.code.span(location);
            let source_span: SourceSpan = source_text.into();
            let pool = self.graph.code.source();
            let source_text = pool.read_span(&source_span, 0, 0).unwrap();
            let source_data = source_text.data();
            let source_text = String::from_utf8_lossy(source_data);
            self.stmt(Statement::Comment(source_text.to_string()));
        }
        match &component.kind {
            ComponentKind::Constant(value) => {
                self.stmt(assign(&node(index), constant(*value)));
            }
            ComponentKind::BitString(bs) => {
                self.stmt(assign(&node(index), bit_string(bs)));
            }
            ComponentKind::Buffer(_) => self.buffer_assign_statement(index)?,
            ComponentKind::Select => self.select_assign_statement(index)?,
            ComponentKind::Binary(_) => self.binary_assign_statement(index)?,
            ComponentKind::Unary(_) => self.unary_assign_statement(index)?,
            ComponentKind::BBInput(_) => self.dff_input_assign_statement(index)?,
            ComponentKind::BBOutput(_) => {}
            ComponentKind::Case(_) => self.case_statement(index)?,
            ComponentKind::DynamicIndex(_) => self.dynamic_index_assign_statement(index)?,
            ComponentKind::DynamicSplice(_) => self.dynamic_splice(index)?,
            ComponentKind::BitSelect(_) => self.bit_select(index)?,
        }
        Ok(())
    }
    pub fn build(mut self) -> Result<Module, RHDLError> {
        let fg = self.graph;
        let ports = fg
            .inputs
            .iter()
            .enumerate()
            .flat_map(|(ndx, x)| {
                (!x.is_empty()).then(|| {
                    port(
                        &format!("arg_{ndx}"),
                        Direction::Input,
                        HDLKind::Wire,
                        unsigned_width(x.len()),
                    )
                })
            })
            .chain(once(port(
                "out",
                Direction::Output,
                HDLKind::Reg,
                unsigned_width(fg.output.len()),
            )))
            .collect();
        let mut declarations = fg
            .graph
            .node_indices()
            .filter_map(|ndx| generate_reg_declaration(ndx, &fg.graph[ndx]))
            .collect::<Vec<_>>();
        let mut statements = vec![];
        let mut submodules = vec![];
        for (ndx, bb) in fg.black_boxes.iter().enumerate() {
            // We need to instantiate the black box
            // Connect the output of the black box
            let mut connections = vec![connection(
                "o",
                concatenate(nodes(bb.outputs.iter().rev().copied().collect())),
            )];
            let inputs = bb
                .inputs
                .iter()
                .map(|x| concatenate(nodes(x.iter().rev().copied().collect())))
                .collect::<Vec<_>>();
            if bb.mode == BlackBoxMode::Synchronous {
                connections.push(connection("clock_reset", inputs[0].clone()));
                connections.push(connection("i", inputs[1].clone()));
            } else {
                connections.push(connection("i", inputs[0].clone()));
            }
            statements.push(component_instance(
                &bb.code.name,
                &format!("bb_{}", ndx),
                connections,
            ));
            submodules.extend([bb.code.as_module()]);
        }
        let mut topo = petgraph::visit::Topo::new(&fg.graph);
        while let Some(ndx) = topo.next(&fg.graph) {
            self.component(ndx)?;
        }
        let output_bits = concatenate(
            fg.output
                .iter()
                .rev()
                .map(|ndx| id(&node(*ndx)))
                .collect::<Vec<_>>(),
        );
        self.stmt(assign("out", output_bits));
        // Check if any of the inputs are used by the body of the module
        let inputs_used = fg.inputs.iter().flatten().any(|node| {
            fg.graph
                .edges_directed(*node, petgraph::Direction::Outgoing)
                .count()
                > 0
        });
        if inputs_used {
            statements.push(always(vec![Events::Star], self.body));
        } else {
            statements.push(initial(self.body));
        }
        declarations.extend(self.decls);
        Ok(Module {
            name: self.name,
            ports,
            declarations,
            statements,
            submodules,
            ..Default::default()
        })
    }
}

pub(crate) fn generate_hdl(module_name: &str, fg: &FlowGraph) -> Result<Module, RHDLError> {
    FlowGraphHDLBuilder::new(module_name, fg).build()
}
