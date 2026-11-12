#[istmo::message]
#[derive(Clone, Debug)]
pub struct Login {
    pub user: String,
    #[redact]
    pub password: String,
}

fn main() {}
