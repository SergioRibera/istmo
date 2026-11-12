#[istmo::service(name = "ui.service_receiver")]
pub trait Background {
    async fn on_start(&mut self, ctx: istmo::plugins::ServiceContext);
}

fn main() {}
