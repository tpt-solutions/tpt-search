//! The optimizer keeps results the same while reordering work, and streaming delivers
//! the same documents in batches.

use tpt_search_core::{Analyzer, Document, FieldSchema, FieldType, Schema};
use tpt_search_query::{
    parse_query, IndexRegistry, PlanNode, QueryExecutor, QueryPlan, QueryPlanner,
};

fn registry() -> IndexRegistry {
    let schema = Schema::new("docs", "id")
        .with_field(FieldSchema::new("id", FieldType::Text))
        .unwrap()
        .with_field(
            FieldSchema::new("title", FieldType::Text)
                .indexed()
                .with_analyzer(Analyzer::english()),
        )
        .unwrap();
    let mut registry = IndexRegistry::new();
    registry.create("docs", schema).unwrap();
    let c = registry.get_mut("docs").unwrap();
    for i in 0..10 {
        let title = if i == 0 {
            "common rare marker"
        } else {
            "common words here"
        };
        c.upsert(&Document::new(format!("d{i}")).with_field("title", title))
            .unwrap();
    }
    registry
}

fn plan_for(registry: &IndexRegistry, text: &str) -> QueryPlan {
    let query = parse_query(text).unwrap();
    QueryPlanner::new()
        .plan("docs", registry.get("docs").unwrap(), &query)
        .unwrap()
}

fn ids(registry: &IndexRegistry, plan: &QueryPlan) -> Vec<String> {
    let mut v: Vec<String> = QueryExecutor::new(registry)
        .execute(plan)
        .unwrap()
        .documents
        .iter()
        .map(|d| d.id.as_str().to_owned())
        .collect();
    v.sort();
    v
}

/// Returns the terms of the first child of the root intersection, if the plan has one.
fn first_intersect_child_terms(plan: &QueryPlan) -> Option<Vec<String>> {
    let body = match &plan.root {
        PlanNode::Sort { input, .. } => input.as_ref(),
        other => other,
    };
    let PlanNode::Intersect(children) = body else {
        return None;
    };
    let mut terms = Vec::new();
    fn collect(node: &PlanNode, out: &mut Vec<String>) {
        match node {
            PlanNode::IndexLookup { term, .. } => out.push(term.text.clone()),
            PlanNode::Union(children) | PlanNode::Intersect(children) => {
                children.iter().for_each(|c| collect(c, out))
            }
            _ => {}
        }
    }
    collect(&children[0], &mut terms);
    Some(terms)
}

#[test]
fn intersections_run_the_most_selective_term_first_and_keep_results() {
    let reg = registry();
    let plan = plan_for(&reg, r#"MATCH(title, "common") AND MATCH(title, "rare")"#);
    let before = ids(&reg, &plan);

    let optimized = QueryPlanner::new().optimize(plan.clone(), reg.get("docs").unwrap());
    assert_eq!(
        first_intersect_child_terms(&optimized),
        Some(vec!["rare".to_string()])
    );
    assert_eq!(ids(&reg, &optimized), before);
    assert_eq!(before, ["d0"]);
}

#[test]
fn an_intersection_with_a_stopword_only_branch_matches_nothing() {
    let reg = registry();
    // "the" is a stop word, so its branch is empty and the whole intersection is empty.
    let plan = plan_for(&reg, r#"MATCH(title, "common") AND MATCH(title, "the")"#);
    let optimized = QueryPlanner::new().optimize(plan.clone(), reg.get("docs").unwrap());
    assert_eq!(ids(&reg, &plan), Vec::<String>::new());
    assert_eq!(ids(&reg, &optimized), Vec::<String>::new());
    assert!(matches!(
        optimized.root,
        PlanNode::Sort { ref input, .. } if matches!(**input, PlanNode::Union(ref v) if v.is_empty())
    ));
}

#[test]
fn optimizing_a_disjunction_removes_duplicate_branches() {
    let reg = registry();
    let plan = plan_for(&reg, r#"MATCH(title, "rare") OR MATCH(title, "rare")"#);
    let before = ids(&reg, &plan);
    let optimized = QueryPlanner::new().optimize(plan, reg.get("docs").unwrap());
    assert_eq!(ids(&reg, &optimized), before);
    assert_eq!(before, ["d0"]);
}

#[test]
fn streaming_delivers_every_document_in_batches() {
    let reg = registry();
    let plan = plan_for(&reg, r#"MATCH(title, "common")"#);
    let streamed = QueryExecutor::new(&reg)
        .execute_streaming(&plan, 4)
        .unwrap();
    assert_eq!(streamed.total_hits, 10);
    let sizes: Vec<usize> = streamed.map(|batch| batch.len()).collect();
    assert_eq!(sizes, [4, 4, 2]);
}

#[test]
fn streaming_with_a_zero_batch_size_still_delivers_everything() {
    let reg = registry();
    let plan = plan_for(&reg, r#"MATCH(title, "rare")"#);
    let total: usize = QueryExecutor::new(&reg)
        .execute_streaming(&plan, 0)
        .unwrap()
        .map(|b| b.len())
        .sum();
    assert_eq!(total, 1);
}
