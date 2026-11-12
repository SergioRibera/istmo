#[istmo::plugin(id = "ui.unknown_argument")]
pub trait Echo {
    async fn echo(&self, text: String) -> String;
}

fn main() {}
