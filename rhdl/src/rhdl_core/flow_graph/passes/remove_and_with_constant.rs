use petgraph::visit::EdgeRef;

use crate::rhdl_core::{
    bitx::BitX,
    flow_graph::{
        component::{Binary, ComponentKind},
        edge_kind::EdgeKind,
        flow_graph_impl::{FlowIx, GraphType},
    },
    hdl::ast::SignedWidth,
    rtl::spec::AluBinary,
    FlowGraph, RHDLError,
};

use super::pass::Pass;

#[derive(Default, Debug, Clone)]
pub struct RemoveAndWithConstantPass {}

struct Replacement {
    original: FlowIx,
    replacement: FlowIx,
}

#[derive(Debug)]
enum VarOrConstant {
    Variable,
    Constant(BitX),
}

fn get_source_bit(node: FlowIx, graph: &GraphType) -> VarOrConstant {
    match graph[node].kind {
        ComponentKind::Constant(value) => VarOrConstant::Constant(value),
        _ => VarOrConstant::Variable,
    }
}

fn get_useless_and_replacement(node: FlowIx, graph: &GraphType) -> Option<FlowIx> {
    let ComponentKind::Binary(Binary {
        op: AluBinary::BitAnd,
        left_len: SignedWidth::Unsigned(1),
        right_len: SignedWidth::Unsigned(1),
    }) = &graph[node].kind
    else {
        return None;
    };
    let left_input = graph
        .edges_directed(node, petgraph::Direction::Incoming)
        .find(|edge| matches!(edge.weight(), EdgeKind::ArgBit(0, 0)))?;
    let right_input = graph
        .edges_directed(node, petgraph::Direction::Incoming)
        .find(|edge| matches!(edge.weight(), EdgeKind::ArgBit(1, 0)))?;
    let left_voc = get_source_bit(left_input.source(), graph);
    let right_voc = get_source_bit(right_input.source(), graph);
    match (left_voc, right_voc) {
        (VarOrConstant::Variable, VarOrConstant::Constant(BitX::Zero)) => {
            Some(right_input.source())
        }
        (VarOrConstant::Variable, VarOrConstant::Constant(BitX::One)) => Some(left_input.source()),
        (VarOrConstant::Constant(BitX::Zero), VarOrConstant::Variable) => Some(left_input.source()),
        (VarOrConstant::Constant(BitX::One), VarOrConstant::Variable) => Some(right_input.source()),
        _ => None,
    }
}

impl Pass for RemoveAndWithConstantPass {
    fn run(mut input: FlowGraph) -> Result<FlowGraph, RHDLError> {
        let mut graph = std::mem::take(&mut input.graph);
        // candidates is a list of original, replacement pairs
        let candidates = graph
            .node_indices()
            .flat_map(|node| {
                get_useless_and_replacement(node, &graph).map(|x| Replacement {
                    original: node,
                    replacement: x,
                })
            })
            .collect::<Vec<_>>();
        // Drop all incoming edges to the set of candidates
        let edges_to_drop = candidates
            .iter()
            .flat_map(|job| {
                graph
                    .edges_directed(job.original, petgraph::Direction::Incoming)
                    .map(|edge| edge.id())
            })
            .collect::<Vec<_>>();
        graph.retain_edges(|_, edge| !edges_to_drop.contains(&edge));
        // Replace each candidate with a single bit buffer
        for job in candidates {
            graph.node_weight_mut(job.original).unwrap().kind =
                ComponentKind::Buffer(format!("and_repaced_{node:?}", node = job.original));
            graph.add_edge(job.replacement, job.original, EdgeKind::ArgBit(0, 0));
        }
        Ok(FlowGraph { graph, ..input })
    }
}
