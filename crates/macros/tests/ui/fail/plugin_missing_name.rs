#[istmo::plugin]
pub trait Echo {
    async fn echo(&self, text: String) -> String;
}

fn main() {}
