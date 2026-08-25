//! `graph` — the GraphRAG CLI over the AEONRIFT repo knowledge graph.
//!
//! Usage:
//!   graph deps <crate>            forward DEPENDS_ON closure (what it needs)
//!   graph rdeps <crate>           reverse DEPENDS_ON closure (blast radius)
//!   graph champion <slug>         a champion's k-hop subgraph (default 3 hops)
//!   graph missing                 completeness critic (no model/faction/test)
//!   graph ask "<question>"        GraphRAG: grounded NL answer over the graph
//!   graph stats                   node/edge counts per kind
//!   graph export --format json|mermaid|dot
//!
//! The repo root is auto-located (or set `AEONRIFT_REPO`).

use std::process::ExitCode;

use aeonrift_graph::query::{self, ExportFormat};
use aeonrift_graph::{build_validated, find_repo_root};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");

    let repo_root = find_repo_root();
    let (graph, report) = match build_validated(&repo_root) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "error: failed to build graph from {}: {e}",
                repo_root.display()
            );
            return ExitCode::FAILURE;
        }
    };

    // The quality gate always runs; a dirty graph is a hard error for every command.
    if !report.is_clean() {
        eprintln!(
            "QUALITY GATE FAILED: {} edge(s) violate domain/range:",
            report.rejected.len()
        );
        for r in &report.rejected {
            eprintln!("  {}", r.describe());
        }
        return ExitCode::FAILURE;
    }

    match cmd {
        "deps" => cmd_closure(&graph, args.get(1), false),
        "rdeps" => cmd_closure(&graph, args.get(1), true),
        "champion" => cmd_champion(&graph, args.get(1)),
        "missing" => cmd_missing(&graph),
        "ask" => cmd_ask(&graph, &args),
        "stats" => cmd_stats(&graph),
        "export" => cmd_export(&graph, &args),
        "" | "help" | "-h" | "--help" => {
            print_usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn cmd_closure(graph: &aeonrift_graph::Graph, name: Option<&String>, reverse: bool) -> ExitCode {
    let Some(name) = name else {
        eprintln!(
            "usage: graph {} <crate>",
            if reverse { "rdeps" } else { "deps" }
        );
        return ExitCode::FAILURE;
    };
    let Some(id) = query::resolve_crate(graph, name) else {
        eprintln!("no such crate: {name}");
        return ExitCode::FAILURE;
    };
    let result = if reverse {
        query::rdeps(graph, &id)
    } else {
        query::deps(graph, &id)
    };
    let label = if reverse { "impacted by" } else { "depends on" };
    println!("{id} — {label} ({} crate(s)):", result.len());
    for c in result {
        println!("  {c}");
    }
    ExitCode::SUCCESS
}

fn cmd_champion(graph: &aeonrift_graph::Graph, slug: Option<&String>) -> ExitCode {
    let Some(slug) = slug else {
        eprintln!("usage: graph champion <slug>");
        return ExitCode::FAILURE;
    };
    let edges = query::champion_subgraph(graph, slug, 3);
    if edges.is_empty() {
        eprintln!("no such champion: {slug}");
        return ExitCode::FAILURE;
    }
    println!("champion '{slug}' — {}-node subgraph:", edges.len());
    for e in edges {
        println!(
            "  {:?}:{} -{}-> {:?}:{}",
            e.from.kind,
            e.from.id,
            e.rel.verb(),
            e.to.kind,
            e.to.id,
        );
    }
    ExitCode::SUCCESS
}

fn cmd_missing(graph: &aeonrift_graph::Graph) -> ExitCode {
    let gaps = query::missing(graph);
    if gaps.is_empty() {
        println!("completeness critic: all champions have a model, a faction, and a test. Nothing missing.");
    } else {
        println!(
            "completeness critic: {} champion(s) incomplete:",
            gaps.len()
        );
        for g in gaps {
            println!("  {} — missing: {}", g.slug, g.missing.join(", "));
        }
    }
    ExitCode::SUCCESS
}

fn cmd_ask(graph: &aeonrift_graph::Graph, args: &[String]) -> ExitCode {
    // Everything after `ask` is the question (quoted or not).
    let question = args[1..].join(" ");
    if question.trim().is_empty() {
        eprintln!("usage: graph ask \"<question>\"");
        return ExitCode::FAILURE;
    }

    // With the `llm` feature and an API key present, produce a grounded answer.
    #[cfg(feature = "llm")]
    {
        match aeonrift_graph::rag::llm::DefaultAsk::from_env() {
            Ok(deps) => {
                let ans = aeonrift_graph::rag::answer(graph, &question, &deps);
                println!("{}", ans.answer);
                if !ans.citations.is_empty() {
                    println!("\ncitations ({} grounded):", ans.citations.len());
                    for c in &ans.citations {
                        println!("  {c}");
                    }
                }
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("llm unavailable ({e}); showing the retrieved subgraph instead:\n");
            }
        }
    }

    // No LLM: still useful — print the retrieved + serialized subgraph so a
    // human (or another agent) can read the grounded evidence directly.
    let sub = aeonrift_graph::rag::retrieve(graph, &question);
    if sub.is_empty() {
        println!("no subgraph resolved for: {question}");
        println!("(mention a champion slug, a crate name, or ask what is missing)");
    } else {
        print!("{}", aeonrift_graph::rag::serialize_for_llm(&sub));
    }
    ExitCode::SUCCESS
}

fn cmd_stats(graph: &aeonrift_graph::Graph) -> ExitCode {
    let s = query::stats(graph);
    println!("NODES ({} total):", s.node_total);
    for (kind, count) in &s.nodes {
        println!("  {:>4}  {:?}", count, kind);
    }
    println!("EDGES ({} total):", s.edge_total);
    for (rel, count) in &s.edges {
        println!("  {:>4}  {}", count, rel.verb());
    }
    ExitCode::SUCCESS
}

fn cmd_export(graph: &aeonrift_graph::Graph, args: &[String]) -> ExitCode {
    // Accept `export --format X` or `export X`.
    let fmt_str = match args.iter().position(|a| a == "--format") {
        Some(i) => args.get(i + 1).map(String::as_str),
        None => args.get(1).map(String::as_str),
    };
    let Some(fmt_str) = fmt_str else {
        eprintln!("usage: graph export --format json|mermaid|dot");
        return ExitCode::FAILURE;
    };
    let Some(fmt) = ExportFormat::parse(fmt_str) else {
        eprintln!("unknown format: {fmt_str} (want json|mermaid|dot)");
        return ExitCode::FAILURE;
    };
    println!("{}", query::export(graph, fmt));
    ExitCode::SUCCESS
}

fn print_usage() {
    eprintln!(
        "graph — GraphRAG over the AEONRIFT repo knowledge graph\n\
         \n\
         commands:\n\
         \x20 deps <crate>       forward DEPENDS_ON closure (what it needs)\n\
         \x20 rdeps <crate>      reverse DEPENDS_ON closure (blast radius)\n\
         \x20 champion <slug>    a champion's 3-hop subgraph\n\
         \x20 missing            completeness critic (no model/faction/test)\n\
         \x20 ask \"<question>\"   GraphRAG: grounded NL answer (or the subgraph without --features llm)\n\
         \x20 stats              node/edge counts per kind\n\
         \x20 export --format json|mermaid|dot"
    );
}
