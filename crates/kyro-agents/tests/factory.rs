//! Full local P3 -> P1 worker -> gVisor builder -> separate attestor -> verified artifact.
//! Model responses and catalogue qualifications are explicitly synthetic here.
#[path = "support/candidate.rs"]
mod candidate;
#[path = "support/fixture.rs"]
mod fixture;

#[tokio::test]
#[ignore = "requires dedicated PostgreSQL roles, real prepared gVisor/Docker controller and compiled attestor"]
async fn agents_build_an_independently_verified_candidate() {
    candidate::verify(fixture::Fixture::real_factory().await, false).await;
}
