//! Runs a suite's cases in order, each isolated from the others' panics,
//! like the unittest and script suites these replace. A case fails by
//! panicking (an `assert!` or an `expect`); the suite fails if any case does.
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;

pub struct Case {
    name: String,
    run: Box<dyn Fn()>,
}

pub fn case(name: impl Into<String>, run: impl Fn() + 'static) -> Case {
    Case { name: name.into(), run: Box::new(run) }
}

/// Runs every case whose name contains one of `filters` (all cases when
/// `filters` is empty) and prints `PASS: summary` when all of them pass. A
/// filter that matches no case is an error, so a renamed case cannot
/// silently drop out of a gate.
pub fn run(suite: &str, summary: &str, cases: Vec<Case>, filters: &[String]) -> ExitCode {
    for filter in filters {
        if !cases.iter().any(|case| case.name.contains(filter.as_str())) {
            eprintln!("{suite}: no case matches {filter:?}");
            return ExitCode::from(2);
        }
    }
    let selected: Vec<&Case> = cases
        .iter()
        .filter(|case| filters.is_empty() || filters.iter().any(|filter| case.name.contains(filter.as_str())))
        .collect();
    let mut failed = Vec::new();
    for case in &selected {
        let started = std::time::Instant::now();
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| (case.run)()));
        let elapsed = started.elapsed().as_secs_f64();
        match outcome {
            Ok(()) => eprintln!("ok {} ({elapsed:.1}s)", case.name),
            Err(_) => {
                eprintln!("FAILED {} ({elapsed:.1}s)", case.name);
                failed.push(case.name.as_str());
            }
        }
    }
    if failed.is_empty() {
        println!("PASS: {summary}");
        ExitCode::SUCCESS
    } else {
        eprintln!("{suite}: {} of {} failed: {}", failed.len(), selected.len(), failed.join(", "));
        ExitCode::FAILURE
    }
}
