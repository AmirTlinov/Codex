use codex_protocol::config_types::ReviewHybridPolicy;
use codex_protocol::config_types::ReviewMode;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SliceGateRecord {
    pub(crate) slice_id: String,
    pub(crate) context_approved: bool,
    pub(crate) verify_green: bool,
    pub(crate) reviewer_approved: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrPackageManifest {
    pub(crate) wave_id: String,
    pub(crate) slices: Vec<SliceGateRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReviewChannels {
    pub(crate) local_pass: bool,
    pub(crate) remote_pass: bool,
}

#[allow(dead_code)]
pub(crate) fn validate_pr_package_manifest(manifest: &PrPackageManifest) -> Result<(), String> {
    if manifest.wave_id.trim().is_empty() {
        return Err("PR package manifest requires a non-empty wave_id.".to_string());
    }
    if manifest.slices.is_empty() {
        return Err("PR package manifest requires at least one slice.".to_string());
    }

    let mut failures = Vec::new();
    for slice in &manifest.slices {
        if slice.slice_id.trim().is_empty() {
            failures.push("slice id is empty".to_string());
            continue;
        }
        if !slice.context_approved {
            failures.push(format!("{}: context not approved", slice.slice_id));
        }
        if !slice.verify_green {
            failures.push(format!("{}: verify is not green", slice.slice_id));
        }
        if !slice.reviewer_approved {
            failures.push(format!(
                "{}: reviewer verdict is not APPROVED",
                slice.slice_id
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "PR package manifest validation failed: {}",
            failures.join("; ")
        ))
    }
}

pub(crate) fn enforce_review_mode_gate(
    mode: ReviewMode,
    hybrid_policy: ReviewHybridPolicy,
    channels: ReviewChannels,
) -> Result<(), String> {
    match mode {
        ReviewMode::Local => {
            if channels.local_pass {
                Ok(())
            } else {
                Err("review.mode=local requires local review PASS".to_string())
            }
        }
        ReviewMode::Remote => {
            if channels.remote_pass {
                Ok(())
            } else {
                Err("review.mode=remote requires remote review PASS".to_string())
            }
        }
        ReviewMode::Hybrid => match hybrid_policy {
            ReviewHybridPolicy::LocalFirst => {
                if channels.local_pass {
                    Ok(())
                } else {
                    Err("review.mode=hybrid (local_first) requires local review PASS".to_string())
                }
            }
            ReviewHybridPolicy::RemoteFirst => {
                if channels.remote_pass {
                    Ok(())
                } else {
                    Err("review.mode=hybrid (remote_first) requires remote review PASS".to_string())
                }
            }
            ReviewHybridPolicy::RequiredBoth => {
                if channels.local_pass && channels.remote_pass {
                    Ok(())
                } else {
                    Err("review.mode=hybrid (required_both) requires local+remote PASS".to_string())
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy_manifest() -> PrPackageManifest {
        PrPackageManifest {
            wave_id: "wave-1".to_string(),
            slices: vec![
                SliceGateRecord {
                    slice_id: "S7".to_string(),
                    context_approved: true,
                    verify_green: true,
                    reviewer_approved: true,
                },
                SliceGateRecord {
                    slice_id: "S8".to_string(),
                    context_approved: true,
                    verify_green: true,
                    reviewer_approved: true,
                },
            ],
        }
    }

    #[test]
    fn unified_pr_package_validator() {
        let manifest = healthy_manifest();
        assert!(validate_pr_package_manifest(&manifest).is_ok());

        let mut broken = manifest;
        broken.slices[1].reviewer_approved = false;
        let err = validate_pr_package_manifest(&broken).expect_err("manifest must fail");
        assert!(err.contains("S8: reviewer verdict is not APPROVED"));
    }

    #[test]
    fn review_mode_local_remote_hybrid_gate() {
        let pass_all = ReviewChannels {
            local_pass: true,
            remote_pass: true,
        };

        assert!(
            enforce_review_mode_gate(ReviewMode::Local, ReviewHybridPolicy::LocalFirst, pass_all)
                .is_ok()
        );
        assert!(
            enforce_review_mode_gate(ReviewMode::Remote, ReviewHybridPolicy::LocalFirst, pass_all)
                .is_ok()
        );
        assert!(
            enforce_review_mode_gate(ReviewMode::Hybrid, ReviewHybridPolicy::LocalFirst, pass_all)
                .is_ok()
        );
        assert!(
            enforce_review_mode_gate(
                ReviewMode::Hybrid,
                ReviewHybridPolicy::RemoteFirst,
                pass_all,
            )
            .is_ok()
        );
        assert!(
            enforce_review_mode_gate(
                ReviewMode::Hybrid,
                ReviewHybridPolicy::RequiredBoth,
                pass_all,
            )
            .is_ok()
        );

        let local_only = ReviewChannels {
            local_pass: true,
            remote_pass: false,
        };
        assert!(
            enforce_review_mode_gate(
                ReviewMode::Hybrid,
                ReviewHybridPolicy::LocalFirst,
                local_only,
            )
            .is_ok()
        );
        assert!(
            enforce_review_mode_gate(
                ReviewMode::Hybrid,
                ReviewHybridPolicy::RemoteFirst,
                local_only,
            )
            .is_err()
        );
        assert!(
            enforce_review_mode_gate(
                ReviewMode::Hybrid,
                ReviewHybridPolicy::RequiredBoth,
                local_only,
            )
            .is_err()
        );
    }
}
