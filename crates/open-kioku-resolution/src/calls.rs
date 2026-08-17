use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use crate::pipeline::ResolutionOutcome;
use open_kioku_core::{CallSite, ReceiverKind};

/// Resolve a call without discarding candidate/proof information.
///
/// This is the canonical RI3 entry point. Structural callers should consume this outcome directly;
/// `resolve_call` remains as the backward-compatible adapter for existing public API consumers.
pub fn resolve_call_outcome(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionOutcome {
    match call.receiver_kind {
        ReceiverKind::Self_ => crate::self_calls::resolve_self_member_outcome(call, ctx),
        ReceiverKind::Super => crate::self_calls::resolve_super_member_outcome(call, ctx),
        ReceiverKind::Type => crate::typed_calls::resolve_static_member_outcome(call, ctx),
        ReceiverKind::Value => crate::typed_calls::resolve_typed_receiver_outcome(call, ctx),
        ReceiverKind::None => crate::bare_calls::resolve_bare_call_outcome(call, ctx),
        ReceiverKind::Module => crate::typed_calls::resolve_module_member_outcome(call, ctx),
        ReceiverKind::Unknown => ResolutionOutcome::Unresolved {
            candidates: Vec::new(),
            reason: "unsupported dynamic/unknown receiver cannot be proven structurally".into(),
        },
    }
}

pub fn resolve_call(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    match call.receiver_kind {
        ReceiverKind::Unknown => ResolutionResult::Unresolved {
            reason: UnresolvedReason::UnsupportedDynamicDispatch,
            evidence: vec![],
        },
        _ => resolve_call_outcome(call, ctx).into_legacy_result(),
    }
}
