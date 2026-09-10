//! Desktop-only in-process "native emulator".
//!
//! On mobile the Kotlin `DataStoreDispatcher` / Swift `DataStoreDispatcher`
//! answer outbound envelopes from the runtime. On desktop we stand in with
//! a plain Rust thread that:
//!
//! * Reads the runtime's outbound envelopes.
//! * Handles `Frame::CreateInstance` by allocating a fresh `Namespace`
//!   keyed by `InstanceId` — same shape as the Kotlin
//!   `DataStoreFactoryImpl.create(config)` reference impl.
//! * Handles `Frame::Call` by decoding the arg tuple, mutating the
//!   in-memory `HashMap`, and encoding the response.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use bincode::{Decode, Encode};
use istmo_core::{CallId, Envelope, Frame, InstanceId, Runtime, codec};
use istmo_data_store::{DATA_STORE_PLUGIN_ID, DataStoreConfig, DataStoreError};

/// Value stored under one key. Matches the plugin's typed accessors so the
/// emulator can enforce type consistency (a `get_i64` against a slot last
/// written by `set_string` returns `DataStoreError::Corrupted`).
#[derive(Debug, Clone, Encode, Decode)]
enum StoredValue {
    Str(String),
    I64(i64),
    F64(f64),
    Bool(bool),
    Bytes(Vec<u8>),
}

impl StoredValue {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Str(_) => "string",
            Self::I64(_) => "i64",
            Self::F64(_) => "f64",
            Self::Bool(_) => "bool",
            Self::Bytes(_) => "bytes",
        }
    }
}

#[derive(Debug, Default)]
struct Namespace {
    #[allow(dead_code)]
    name: String,
    entries: HashMap<String, StoredValue>,
}

/// Spawn the emulator on its own OS thread. Drops when the outbound
/// channel closes (i.e. the runtime tears down).
pub fn spawn(rt: Arc<Runtime>, outbound: flume::Receiver<Envelope>) {
    thread::Builder::new()
        .name("data-store-native-emulator".into())
        .spawn(move || {
            let mut namespaces: HashMap<InstanceId, Namespace> = HashMap::new();
            let mut next_instance: u64 = 1;
            while let Ok(env) = outbound.recv() {
                if let Err(err) = handle_frame(&rt, &mut namespaces, &mut next_instance, env.frame)
                {
                    eprintln!("data-store emulator error: {err:?}");
                }
            }
        })
        .expect("spawn emulator thread");
}

fn handle_frame(
    rt: &Arc<Runtime>,
    namespaces: &mut HashMap<InstanceId, Namespace>,
    next_instance: &mut u64,
    frame: Frame,
) -> Result<(), Box<dyn std::error::Error>> {
    match frame {
        Frame::CreateInstance { call_id, plugin_id, payload } => {
            if plugin_id != DATA_STORE_PLUGIN_ID {
                respond_err(rt, call_id, "unknown plugin id")?;
                return Ok(());
            }
            let (cfg, _) = codec::decode::<DataStoreConfig>(&payload)?;
            let iid = InstanceId(*next_instance);
            *next_instance += 1;
            namespaces
                .insert(iid, Namespace { name: cfg.namespace, entries: HashMap::new() });
            rt.dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(codec::encode(&iid)?),
            }))?;
        }
        Frame::Call { call_id, plugin_id, instance_id, method, payload } => {
            if plugin_id != DATA_STORE_PLUGIN_ID {
                respond_err(rt, call_id, "unknown plugin id")?;
                return Ok(());
            }
            let Some(iid) = instance_id else {
                respond_err(rt, call_id, "missing instance id")?;
                return Ok(());
            };
            let Some(ns) = namespaces.get_mut(&iid) else {
                respond_err(rt, call_id, "unknown instance id")?;
                return Ok(());
            };
            let result = dispatch_method(ns, &method, &payload);
            rt.dispatch_inbound(Envelope::new(Frame::Respond { call_id, result }))?;
        }
        Frame::DestroyInstance { instance_id } => {
            namespaces.remove(&instance_id);
        }
        _ => {}
    }
    Ok(())
}

fn respond_err(
    rt: &Arc<Runtime>,
    call_id: CallId,
    msg: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let err = DataStoreError::Backend(msg.to_owned());
    rt.dispatch_inbound(Envelope::new(Frame::Respond {
        call_id,
        result: Err(codec::encode(&err)?),
    }))?;
    Ok(())
}

fn dispatch_method(ns: &mut Namespace, method: &str, payload: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    match method {
        "get_string" => decode_get(payload, &ns.entries, |v| match v {
            StoredValue::Str(s) => Ok(s.clone()),
            other => Err(type_mismatch("string", other.kind())),
        }),
        "set_string" => decode_set::<String>(payload, &mut ns.entries, StoredValue::Str),
        "get_i64" => decode_get(payload, &ns.entries, |v| match v {
            StoredValue::I64(n) => Ok(*n),
            other => Err(type_mismatch("i64", other.kind())),
        }),
        "set_i64" => decode_set::<i64>(payload, &mut ns.entries, StoredValue::I64),
        "get_f64" => decode_get(payload, &ns.entries, |v| match v {
            StoredValue::F64(n) => Ok(*n),
            other => Err(type_mismatch("f64", other.kind())),
        }),
        "set_f64" => decode_set::<f64>(payload, &mut ns.entries, StoredValue::F64),
        "get_bool" => decode_get(payload, &ns.entries, |v| match v {
            StoredValue::Bool(b) => Ok(*b),
            other => Err(type_mismatch("bool", other.kind())),
        }),
        "set_bool" => decode_set::<bool>(payload, &mut ns.entries, StoredValue::Bool),
        "get_bytes" => decode_get(payload, &ns.entries, |v| match v {
            StoredValue::Bytes(b) => Ok(b.clone()),
            other => Err(type_mismatch("bytes", other.kind())),
        }),
        "set_bytes" => decode_set::<Vec<u8>>(payload, &mut ns.entries, StoredValue::Bytes),
        "remove" => {
            let ((key,), _) = codec::decode::<(String,)>(payload).map_err(encode_backend)?;
            let removed = ns.entries.remove(&key).is_some();
            codec::encode(&removed).map_err(encode_backend)
        }
        "contains" => {
            let ((key,), _) = codec::decode::<(String,)>(payload).map_err(encode_backend)?;
            codec::encode(&ns.entries.contains_key(&key)).map_err(encode_backend)
        }
        "keys" => {
            let keys: Vec<String> = ns.entries.keys().cloned().collect();
            codec::encode(&keys).map_err(encode_backend)
        }
        "clear" => {
            ns.entries.clear();
            codec::encode(&()).map_err(encode_backend)
        }
        other => Err(encode_backend_err(&format!("unknown method: {other}"))),
    }
}

fn decode_get<T: Encode>(
    payload: &[u8],
    entries: &HashMap<String, StoredValue>,
    extract: impl Fn(&StoredValue) -> Result<T, DataStoreError>,
) -> Result<Vec<u8>, Vec<u8>> {
    let ((key,), _) = codec::decode::<(String,)>(payload).map_err(encode_backend)?;
    let value: Option<T> = match entries.get(&key) {
        Some(v) => Some(extract(v).map_err(|e| codec::encode(&e).unwrap_or_default())?),
        None => None,
    };
    codec::encode(&value).map_err(encode_backend)
}

fn decode_set<T: bincode::Decode<()>>(
    payload: &[u8],
    entries: &mut HashMap<String, StoredValue>,
    wrap: impl Fn(T) -> StoredValue,
) -> Result<Vec<u8>, Vec<u8>> {
    let ((key, value), _) = codec::decode::<(String, T)>(payload).map_err(encode_backend)?;
    entries.insert(key, wrap(value));
    codec::encode(&()).map_err(encode_backend)
}

fn type_mismatch(expected: &str, actual: &str) -> DataStoreError {
    DataStoreError::Corrupted(format!("expected {expected}, stored value is {actual}"))
}

fn encode_backend<E: std::fmt::Display>(err: E) -> Vec<u8> {
    encode_backend_err(&err.to_string())
}

fn encode_backend_err(msg: &str) -> Vec<u8> {
    codec::encode(&DataStoreError::Backend(msg.to_owned())).unwrap_or_default()
}
