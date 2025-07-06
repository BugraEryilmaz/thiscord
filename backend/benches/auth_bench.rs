
use backend::create_router_with_state;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tower::util::ServiceExt;
use rand::Rng;
use tokio::{runtime::Runtime};

fn bench_signup(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let router = create_router_with_state(&rt);
    c.bench_function("signup", |b| {
        b.to_async(&rt).iter_batched(|| {
            let mut rng = rand::rng();
            let uri = "/auth/signup";
            let random: u64 = rng.random_range(0..u64::MAX);
            let username: String = format!("testuser_{}", random);
            let email: String = format!("testuser_{}@example.com", random);
            let password: String = "TestPassword".into();
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(format!(r#"{{"username":"{}","email":"{}","password":"{}"}}"#, username, email, password)))
                .unwrap();
            black_box(request)
            }, async |req| {
            let response = router.clone().oneshot(req).await.unwrap();
            if !response.status().is_success() {
                panic!("Signup failed with status: {}", response.status());
            }
        },
        criterion::BatchSize::SmallInput);
    });
}

criterion_group!(benches, bench_signup);
criterion_main!(benches);
