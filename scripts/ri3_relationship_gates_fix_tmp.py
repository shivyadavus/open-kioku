from pathlib import Path

path = Path("crates/open-kioku-cli/src/bench/relationship.rs")
text = path.read_text()
old = '''            let metrics = report.by_language_relationship.get(&key);
            let cases = metrics.map(|value| value.cases).unwrap_or(0);
            if cases < policy.minimum_cases_per_language_relationship {
                failures.push(format!(
                    "cohort {key} has {cases} cases, below required {}",
                    policy.minimum_cases_per_language_relationship
                ));
                continue;
            }
            let metrics = metrics.expect("cohort with cases must have metrics");
'''
new = '''            let Some(metrics) = report.by_language_relationship.get(&key) else {
                if policy.minimum_cases_per_language_relationship > 0 {
                    failures.push(format!(
                        "cohort {key} has 0 cases, below required {}",
                        policy.minimum_cases_per_language_relationship
                    ));
                }
                continue;
            };
            let cases = metrics.cases;
            if cases < policy.minimum_cases_per_language_relationship {
                failures.push(format!(
                    "cohort {key} has {cases} cases, below required {}",
                    policy.minimum_cases_per_language_relationship
                ));
                continue;
            }
'''
observed = text.count(old)
if observed != 1:
    raise SystemExit(f"absent cohort seam changed: expected 1, observed {observed}")
path.write_text(text.replace(old, new, 1))
