use crate::context::{ResolutionContext, ResolutionResult, UnresolvedReason};
use open_kioku_core::{CallSite, ReceiverKind};

pub fn resolve_call(call: &CallSite, ctx: &ResolutionContext<'_>) -> ResolutionResult {
    match call.receiver_kind {
        ReceiverKind::Self_ => {
            crate::self_calls::resolve_self_member_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::Super => {
            crate::self_calls::resolve_super_member_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::Type => {
            crate::typed_calls::resolve_static_member_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::Value => {
            crate::typed_calls::resolve_typed_receiver_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::None => {
            crate::bare_calls::resolve_bare_call_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::Module => {
            crate::typed_calls::resolve_module_member_outcome(call, ctx).into_legacy_result()
        }
        ReceiverKind::Unknown => ResolutionResult::Unresolved {
            reason: UnresolvedReason::UnsupportedDynamicDispatch,
            evidence: vec![],
        },
    }
}
