//! Cost-based plan rewriting.
//!
//! The optimizer works on a finished plan and the statistics of its collection. It never
//! changes what a plan returns. It reorders work so that cheap, selective steps run first,
//! and it removes steps that cannot contribute:
//!
//! * `Intersect` children are ordered by estimated cardinality, smallest first, so the
//!   running candidate set shrinks as early as possible.
//! * A child that matches nothing makes its `Intersect` match nothing.
//! * `Union` flattens nested unions and drops duplicate branches.
//! * A `Difference` that excludes nothing is replaced by its input.
//!
//! Estimates come from the planner's cardinality model over the collection's current
//! posting-list sizes, so they follow the data.

use tpt_search_index::Collection;

use crate::plan::{Ctx, PlanNode, QueryPlan, QueryPlanner};

impl QueryPlanner {
    /// Rewrites a plan using `collection`'s statistics. The returned plan gives the same
    /// results as the input.
    pub fn optimize(&self, plan: QueryPlan, collection: &Collection) -> QueryPlan {
        let ctx = Ctx {
            index: &plan.index,
            collection,
        };
        let root = rewrite(&ctx, plan.root);
        let estimated_cardinality = ctx.cardinality(&root);
        let estimated_cost = ctx.cost(&root);
        QueryPlan {
            index: plan.index,
            root,
            estimated_cost,
            estimated_cardinality,
        }
    }
}

fn is_empty_node(node: &PlanNode) -> bool {
    matches!(node, PlanNode::Union(children) if children.is_empty())
}

fn rewrite(ctx: &Ctx<'_>, node: PlanNode) -> PlanNode {
    match node {
        PlanNode::Intersect(children) => {
            let mut children: Vec<PlanNode> =
                children.into_iter().map(|c| rewrite(ctx, c)).collect();
            if children.iter().any(is_empty_node) {
                return PlanNode::Union(Vec::new());
            }
            // Cheapest-to-evaluate, most selective first.
            children.sort_by_key(|c| ctx.cardinality(c));
            if children.len() == 1 {
                children.remove(0)
            } else {
                PlanNode::Intersect(children)
            }
        }
        PlanNode::Union(children) => {
            let mut flat: Vec<PlanNode> = Vec::new();
            for child in children {
                match rewrite(ctx, child) {
                    PlanNode::Union(inner) => {
                        for c in inner {
                            if !flat.contains(&c) {
                                flat.push(c);
                            }
                        }
                    }
                    other => {
                        if !flat.contains(&other) {
                            flat.push(other);
                        }
                    }
                }
            }
            if flat.len() == 1 {
                flat.remove(0)
            } else {
                PlanNode::Union(flat)
            }
        }
        PlanNode::Difference { input, exclude } => {
            let exclude = rewrite(ctx, *exclude);
            let input = rewrite(ctx, *input);
            if is_empty_node(&exclude) {
                input
            } else {
                PlanNode::Difference {
                    input: Box::new(input),
                    exclude: Box::new(exclude),
                }
            }
        }
        PlanNode::Fusion { inputs, method } => PlanNode::Fusion {
            inputs: inputs.into_iter().map(|c| rewrite(ctx, c)).collect(),
            method,
        },
        PlanNode::Sort { input, keys } => PlanNode::Sort {
            input: Box::new(rewrite(ctx, *input)),
            keys,
        },
        PlanNode::Limit { input, limit } => PlanNode::Limit {
            input: Box::new(rewrite(ctx, *input)),
            limit,
        },
        leaf => leaf,
    }
}
