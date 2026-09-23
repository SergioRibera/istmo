use istmo_build::{AppOpts, Contract, EnumDef, EnumVariant, Field, StructDef, TypeDef, TypeRef};

fn main() {
    // `LiveActivity` ships via the standard `DEP_*_ISTMO_CONTRACT` handover;
    // the inline `Demo` bundle carries the shared `TimerAttributes` /
    // `TimerState` types that both the Rust and Swift/Kotlin sides encode.
    istmo_build::emit_with(AppOpts {
        extra_contracts: vec![demo_types_contract()],
        ..AppOpts::default()
    });
}

fn demo_types_contract() -> Contract {
    Contract {
        plugin_id: "demo.live_activity".to_owned(),
        type_name: "Demo".to_owned(),
        methods: vec![],
        init: None,
        types: vec![
            TypeDef::Struct(StructDef {
                name: "TimerAttributes".to_owned(),
                fields: vec![
                    Field {
                        name: "title".to_owned(),
                        ty: TypeRef::String,
                    },
                    Field {
                        name: "target_seconds".to_owned(),
                        ty: TypeRef::U32,
                    },
                ],
            }),
            TypeDef::Struct(StructDef {
                name: "TimerState".to_owned(),
                fields: vec![
                    Field {
                        name: "elapsed_seconds".to_owned(),
                        ty: TypeRef::U32,
                    },
                    Field {
                        name: "label".to_owned(),
                        ty: TypeRef::String,
                    },
                ],
            }),
            TypeDef::Enum(EnumDef {
                name: "TimerCodecMarker".to_owned(),
                variants: vec![
                    EnumVariant {
                        name: "Attributes".to_owned(),
                        payload: vec![TypeRef::Named("TimerAttributes".to_owned())],
                    },
                    EnumVariant {
                        name: "State".to_owned(),
                        payload: vec![TypeRef::Named("TimerState".to_owned())],
                    },
                ],
            }),
        ],
    }
}
