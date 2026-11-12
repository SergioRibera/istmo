//! Randomised interleavings of `BannerSlot::sync` with show / update /
//! hide completions, driven by a manual executor so every ordering of
//! "future polled", "native answered" and "UI synced" gets exercised.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::task::{Context, Waker};
use std::thread;

use istmo_core::{CallId, Envelope, Frame, InstanceId, NativeHandleId, Runtime, codec};
use istmo_plugins::{
    AdMobClient, AdMobConfig, BannerRect, BannerRequest, BannerSlot, BannerSlotBoxFuture,
    SlotStatus, SlotTarget,
};

/// xorshift64*: deterministic per seed, no extra dependency.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }
}

enum Pending {
    Show { rect: BannerRect },
    Update { id: NativeHandleId, rect: BannerRect },
    Hide { id: NativeHandleId },
}

/// The native side as the slot drives it.
#[derive(Default)]
struct Native {
    next_id: u64,
    /// Banners shown and not yet hidden or released, with their rect.
    live: HashMap<NativeHandleId, BannerRect>,
    /// Ids a hide or release was sent for.
    gone: HashSet<NativeHandleId>,
    calls: Vec<(CallId, Pending)>,
}

impl Native {
    /// Record one outbound frame, checking it against what was sent before.
    fn receive(&mut self, env: Envelope, seed: u64) {
        match env.frame {
            Frame::Call {
                call_id,
                method,
                payload,
                ..
            } => {
                let pending = match method.as_str() {
                    "show_banner" => {
                        let ((req,), _) = codec::decode::<(BannerRequest,)>(&payload).unwrap();
                        Pending::Show { rect: req.rect }
                    }
                    "update_banner" => {
                        let ((id, rect), _) =
                            codec::decode::<(NativeHandleId, BannerRect)>(&payload).unwrap();
                        assert!(
                            !self.gone.contains(&id),
                            "seed {seed}: update_banner sent for released banner {id:?}"
                        );
                        Pending::Update { id, rect }
                    }
                    "hide_banner" => {
                        let ((id,), _) = codec::decode::<(NativeHandleId,)>(&payload).unwrap();
                        assert!(
                            self.gone.insert(id),
                            "seed {seed}: banner {id:?} hidden twice"
                        );
                        Pending::Hide { id }
                    }
                    other => panic!("seed {seed}: unexpected call {other}"),
                };
                self.calls.push((call_id, pending));
            }
            Frame::ReleaseNativeHandle { handle_id } => {
                assert!(
                    self.gone.insert(handle_id),
                    "seed {seed}: banner {handle_id:?} released after its hide"
                );
                self.live.remove(&handle_id);
            }
            other => panic!("seed {seed}: unexpected frame {other:?}"),
        }
    }

    /// Answer call `index` successfully.
    fn answer(&mut self, rt: &Arc<Runtime>, index: usize, seed: u64) {
        let (call_id, pending) = self.calls.remove(index);
        let payload = match pending {
            Pending::Show { rect } => {
                self.next_id += 1;
                let id = NativeHandleId(self.next_id);
                self.live.insert(id, rect);
                assert!(
                    self.live.len() <= 1,
                    "seed {seed}: two banners on screen at once"
                );
                codec::encode(&id).unwrap()
            }
            Pending::Update { id, rect } => {
                if let Some(current) = self.live.get_mut(&id) {
                    *current = rect;
                }
                Vec::new()
            }
            Pending::Hide { id } => {
                self.live.remove(&id);
                Vec::new()
            }
        };
        rt.dispatch_inbound(Envelope::new(Frame::Respond {
            call_id,
            result: Ok(payload),
        }))
        .unwrap();
    }
}

fn client(rt: &Arc<Runtime>, outbound: &flume::Receiver<Envelope>) -> Arc<AdMobClient> {
    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        let env = backend_outbound.recv().unwrap();
        let Frame::CreateInstance { call_id, .. } = env.frame else {
            panic!("expected CreateInstance");
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(codec::encode(&InstanceId(1)).unwrap()),
            }))
            .unwrap();
    });
    let config = AdMobConfig {
        app_id: "app".to_owned(),
        test_device_ids: Vec::new(),
        child_directed_treatment: false,
    };
    let client = pollster::block_on(AdMobClient::from_runtime_with(rt, config)).unwrap();
    backend.join().unwrap();
    Arc::new(client)
}

const RECTS: [BannerRect; 3] = [
    BannerRect {
        x: 0,
        y: 0,
        width: 320,
        height: 50,
    },
    BannerRect {
        x: 0,
        y: 600,
        width: 320,
        height: 50,
    },
    BannerRect {
        x: 10,
        y: 300,
        width: 300,
        height: 60,
    },
];

fn run(seed: u64) {
    let init = Runtime::mock();
    let rt = init.runtime;
    let outbound = init.outbound;
    let client = client(&rt, &outbound);

    let queue: Arc<Mutex<Vec<BannerSlotBoxFuture>>> = Arc::default();
    let spawn_queue = queue.clone();
    let slot = BannerSlot::new("unit", client)
        .with_spawn(move |fut| spawn_queue.lock().unwrap().push(fut));

    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut native = Native::default();
    let mut futures: Vec<BannerSlotBoxFuture> = Vec::new();
    let mut target = SlotTarget::Hide;
    let mut cx = Context::from_waker(Waker::noop());

    let mut poll_one = |futures: &mut Vec<BannerSlotBoxFuture>, index: usize| {
        if futures[index].as_mut().poll(&mut cx).is_ready() {
            drop(futures.swap_remove(index));
        }
    };

    for _ in 0..200 {
        futures.append(&mut queue.lock().unwrap());
        while let Ok(env) = outbound.try_recv() {
            native.receive(env, seed);
        }
        match rng.below(3) {
            0 => {
                target = if rng.below(3) == 0 {
                    SlotTarget::Hide
                } else {
                    SlotTarget::Show(RECTS[rng.below(RECTS.len())])
                };
                slot.sync(target.clone());
            }
            1 if !futures.is_empty() => {
                let index = rng.below(futures.len());
                poll_one(&mut futures, index);
            }
            2 if !native.calls.is_empty() => {
                let index = rng.below(native.calls.len());
                native.answer(&rt, index, seed);
            }
            _ => {}
        }
    }

    // Quiesce without further syncs: the slot must get there on its own.
    for _ in 0..1000 {
        futures.append(&mut queue.lock().unwrap());
        while let Ok(env) = outbound.try_recv() {
            native.receive(env, seed);
        }
        if futures.is_empty() && native.calls.is_empty() {
            break;
        }
        for index in (0..futures.len()).rev() {
            poll_one(&mut futures, index);
        }
        while !native.calls.is_empty() {
            native.answer(&rt, 0, seed);
        }
    }
    assert!(
        futures.is_empty() && native.calls.is_empty(),
        "seed {seed}: never quiesced"
    );

    match target {
        SlotTarget::Show(rect) => {
            assert_eq!(slot.status(), SlotStatus::Live, "seed {seed}");
            let rects: Vec<_> = native.live.values().copied().collect();
            assert_eq!(rects, [rect], "seed {seed}: banner not at the last rect");
        }
        SlotTarget::Hide => {
            assert_eq!(slot.status(), SlotStatus::Idle, "seed {seed}");
            assert!(native.live.is_empty(), "seed {seed}: banner left on screen");
        }
    }
}

#[test]
fn random_interleavings_converge_to_the_last_target() {
    for seed in 0..500 {
        run(seed);
    }
}
