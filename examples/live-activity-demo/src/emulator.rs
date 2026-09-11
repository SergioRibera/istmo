//! Desktop-only in-process "native emulator" for the live-activity
//! plugin.
//!
//! On mobile the Kotlin `LiveActivityDispatcher` / Swift
//! `LiveActivityDispatcher` answer outbound envelopes from the runtime.
//! On desktop we stand in with a plain Rust thread that:
//!
//! * Reads every outbound envelope from the runtime.
//! * Handles `Frame::Call` for `istmo.live_activity` by decoding the arg
//!   tuple, mutating an in-memory `HashMap<u64, ActivityRecord>`, and
//!   encoding the response.
//! * Reports a synthetic desktop-only `PlatformCapabilities::Unsupported`
//!   so the UI can distinguish the emulator from a real device.
//!
//! Only the six trait methods that this demo actually exercises are
//! implemented (start / update / end / are_activities_enabled /
//! capabilities / restore_active).

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::sync::atomic::{AtomicU64, Ordering};

use istmo_core::{CallId, Envelope, Frame, NativeHandleId, Runtime, codec};
use istmo_live_activity::{
    ActivityError, ActivityStyle, AlertConfig, AndroidTierHint, DismissalPolicy,
    LIVE_ACTIVITY_PLUGIN_ID, PlatformCapabilities, RestoredActivity,
};

#[derive(Debug, Clone)]
struct ActivityRecord {
    activity_type: String,
    attributes: Vec<u8>,
    state: Vec<u8>,
}

/// Spawn the emulator on its own OS thread. Drops when the outbound
/// channel closes (i.e. the runtime tears down).
pub fn spawn(rt: Arc<Runtime>, outbound: flume::Receiver<Envelope>) {
    let counter = AtomicU64::new(1);
    thread::Builder::new()
        .name("live-activity-native-emulator".into())
        .spawn(move || {
            let mut activities: HashMap<u64, ActivityRecord> = HashMap::new();
            while let Ok(env) = outbound.recv() {
                if let Err(err) = handle_frame(&rt, &counter, &mut activities, env.frame) {
                    eprintln!("live-activity emulator error: {err}");
                }
            }
        })
        .expect("spawn emulator thread");
}

fn handle_frame(
    rt: &Arc<Runtime>,
    counter: &AtomicU64,
    activities: &mut HashMap<u64, ActivityRecord>,
    frame: Frame,
) -> Result<(), Box<dyn std::error::Error>> {
    match frame {
        Frame::Call { call_id, plugin_id, instance_id: _, method, payload } => {
            if plugin_id != LIVE_ACTIVITY_PLUGIN_ID {
                respond_err(rt, call_id, ActivityError::Backend("unknown plugin id".into()))?;
                return Ok(());
            }
            let result = dispatch(method.as_str(), &payload, counter, activities);
            rt.dispatch_inbound(Envelope::new(Frame::Respond { call_id, result }))?;
        }
        Frame::ReleaseNativeHandle { handle_id } => {
            let _ = activities.remove(&handle_id.0);
        }
        _ => {}
    }
    Ok(())
}

fn dispatch(
    method: &str,
    payload: &[u8],
    counter: &AtomicU64,
    activities: &mut HashMap<u64, ActivityRecord>,
) -> Result<Vec<u8>, Vec<u8>> {
    match method {
        "start" => {
            type Args = (
                String,
                Vec<u8>,
                Vec<u8>,
                ActivityStyle,
                Option<u32>,
                Option<AndroidTierHint>,
            );
            let ((activity_type, attributes, initial_state, _style, _stale, _hint), _) =
                codec::decode::<Args>(payload).map_err(encode_backend)?;
            let id = counter.fetch_add(1, Ordering::Relaxed);
            activities.insert(
                id,
                ActivityRecord { activity_type, attributes, state: initial_state },
            );
            codec::encode(&NativeHandleId(id)).map_err(encode_backend)
        }
        "update" => {
            type Args = (NativeHandleId, Vec<u8>, Option<AlertConfig>);
            let ((handle, state, _alert), _) =
                codec::decode::<Args>(payload).map_err(encode_backend)?;
            match activities.get_mut(&handle.0) {
                Some(record) => {
                    record.state = state;
                    codec::encode(&()).map_err(encode_backend)
                }
                None => Err(encode_activity_err(ActivityError::HandleNotFound)),
            }
        }
        "end" => {
            type Args = (NativeHandleId, Option<Vec<u8>>, DismissalPolicy);
            let ((handle, final_state, _dismissal), _) =
                codec::decode::<Args>(payload).map_err(encode_backend)?;
            match activities.remove(&handle.0) {
                Some(_) => {
                    let _ = final_state;
                    codec::encode(&()).map_err(encode_backend)
                }
                None => Err(encode_activity_err(ActivityError::HandleNotFound)),
            }
        }
        "are_activities_enabled" => codec::encode(&true).map_err(encode_backend),
        "capabilities" => codec::encode(&PlatformCapabilities::Unsupported).map_err(encode_backend),
        "restore_active" => {
            let snapshot: Vec<RestoredActivity> = activities
                .iter()
                .map(|(id, rec)| RestoredActivity {
                    handle: NativeHandleId(*id),
                    activity_type: rec.activity_type.clone(),
                    attributes: rec.attributes.clone(),
                    state: rec.state.clone(),
                })
                .collect();
            codec::encode(&snapshot).map_err(encode_backend)
        }
        other => Err(encode_backend_str(&format!("unknown method: {other}"))),
    }
}

fn respond_err(
    rt: &Arc<Runtime>,
    call_id: CallId,
    err: ActivityError,
) -> Result<(), Box<dyn std::error::Error>> {
    rt.dispatch_inbound(Envelope::new(Frame::Respond {
        call_id,
        result: Err(encode_activity_err(err)),
    }))?;
    Ok(())
}

fn encode_activity_err(err: ActivityError) -> Vec<u8> {
    codec::encode(&err).unwrap_or_default()
}

fn encode_backend<E: std::fmt::Display>(err: E) -> Vec<u8> {
    encode_backend_str(&err.to_string())
}

fn encode_backend_str(msg: &str) -> Vec<u8> {
    codec::encode(&ActivityError::Backend(msg.to_owned())).unwrap_or_default()
}
