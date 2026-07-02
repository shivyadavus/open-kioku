fn verify_plan_evidence(report: &PlanReport, mode: EvidenceVerifyMode) -> anyhow::Result<()> {
    if mode == EvidenceVerifyMode::Off {
        return Ok(());
    }
    let missing = report
        .negative_evidence
        .iter()
        .filter(|item| item.confidence >= 0.70)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    for item in &missing {
        let next_probe = item
            .suggested_next_probe
            .as_deref()
            .unwrap_or("collect stronger evidence before editing");
        eprintln!(
            "negative evidence [{}]: {} (confidence {:.2}); next probe: {}",
            item.scope, item.reason, item.confidence, next_probe
        );
    }
    if mode == EvidenceVerifyMode::Fail {
        anyhow::bail!(
            "plan evidence verification failed: {} required evidence signal(s) missing",
            missing.len()
        );
    }
    Ok(())
}
