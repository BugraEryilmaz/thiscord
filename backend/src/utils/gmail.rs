use lettre::{
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    transport::smtp::authentication::Credentials,
};

use crate::Error;

#[derive(Clone, Debug)]
pub struct GmailBackend {
    pub from: String,
    pub mailer: AsyncSmtpTransport<Tokio1Executor>,
}

impl GmailBackend {
    pub fn new() -> Self {
        let from = std::env::var("GMAIL_ADDRESS").expect("GMAIL_ADDRESS must be set");
        let password = std::env::var("GMAIL_PASSWORD").expect("GMAIL_PASSWORD must be set");
        let creds = Credentials::new(from.clone(), password);
        let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay("smtp.gmail.com")
            .unwrap()
            .credentials(creds)
            .build();
        GmailBackend { from, mailer }
    }

    pub async fn send_email(&self, to: &str, subject: &str, body: &str) -> Result<(), Error> {
        let email = lettre::Message::builder()
            .from(self.from.parse().unwrap())
            .to(to.parse().unwrap())
            .subject(subject)
            .body(body.to_string())
            .unwrap();

        self.mailer.send(email).await?;
        Ok(())
    }
}
