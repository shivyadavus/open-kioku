from pathlib import Path

path = Path("scripts/ri3_resolution_telemetry_tmp.py")
text = path.read_text()
old = '''# References are finalized only after optional SCIP import, so telemetry runs over the final occurrence set.
anchor = ''' + "'''" + '''        let repository = Repository {
''' + "'''" + '''
insert = ''' + "'''" + '''        if resolution_mode != open_kioku_config::ResolutionMode::Legacy {
            for occurrence in occurrences.iter().filter(|occurrence| !occurrence.is_definition) {
                if let Some(file) = file_lookup.get(&occurrence.file_id) {
                    quality_report.record_reference_occurrence(
                        file.language.clone(),
                        occurrence.confidence == Confidence::Exact,
                    );
                }
            }
            quality_report.normalize_telemetry();
        }

''' + "'''" + '''
text = replace_exact(text, anchor, insert + anchor, "reference telemetry finalization")
'''
new = '''# References are finalized only after optional SCIP import, so telemetry runs over the final occurrence set.
anchor = ''' + "'''" + '''            scip_report = Some(report);
        }
        let repository = Repository {
''' + "'''" + '''
replacement = ''' + "'''" + '''            scip_report = Some(report);
        }
        if resolution_mode != open_kioku_config::ResolutionMode::Legacy {
            for occurrence in occurrences.iter().filter(|occurrence| !occurrence.is_definition) {
                if let Some(file) = file_lookup.get(&occurrence.file_id) {
                    quality_report.record_reference_occurrence(
                        file.language.clone(),
                        occurrence.confidence == Confidence::Exact,
                    );
                }
            }
            quality_report.normalize_telemetry();
        }

        let repository = Repository {
''' + "'''" + '''
text = replace_exact(text, anchor, replacement, "reference telemetry finalization")
'''
if text.count(old) != 1:
    raise SystemExit(f"telemetry anchor source seam changed: {text.count(old)}")
path.write_text(text.replace(old, new, 1))
