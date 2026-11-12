#[istmo::message]
pub enum Auth {
    Password {
        #[redact]
        value: String,
    },
}

fn main() {}
