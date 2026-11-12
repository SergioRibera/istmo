pub enum Credential {}

#[istmo::message]
pub enum Login {
    Done {
        #[handle(Credential)]
        credential: istmo::NativeHandleId,
    },
    Cancelled,
}

fn main() {}
