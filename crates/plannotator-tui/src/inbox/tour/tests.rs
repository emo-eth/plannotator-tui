use anyhow::Result;

use super::fixture::{DESIGN, Fixture, asset_path};
use super::model::{Replay, Stage, Target};

fn fixture() -> Result<Fixture> {
    let fixture: Fixture = serde_json::from_str(include_str!("fixture.json"))?;
    fixture.validate()?;
    Ok(fixture)
}

#[test]
fn upstream_fixture_questions_retain_real_labels_and_picture_assets() -> Result<()> {
    let fixture = fixture()?;
    let warm = fixture.question(true)?;
    assert_eq!(warm.choices.len(), 2);
    assert_eq!(warm.choices.first().map(|choice| choice.label.as_str()), Some("Yes, start it"));
    let pictures = fixture.question(false)?;
    assert_eq!(pictures.choices.len(), 4);
    assert_eq!(
        pictures.choices.first().map(|choice| choice.label.as_str()),
        Some(fixture.thread(DESIGN)?.real_answer.as_str())
    );
    assert_eq!(
        pictures.choices.first().and_then(|choice| choice.picture.as_deref()),
        Some("vs-a-thumb.webp")
    );
    assert_eq!(fixture.picture("vs-a-thumb.webp"), "vs-a.webp");
    Ok(())
}

#[test]
fn fixture_assets_cannot_escape_into_live_inbox_routes() {
    for path in [
        "../threads",
        "/api/inbox/messages/a/picks",
        "https://example.org/image.webp",
        "a%2fb.webp",
        "x.webp?delete=1",
    ] {
        assert!(asset_path(path).is_err());
    }
    assert_eq!(asset_path("built-a1.webp").ok().as_deref(), Some("/api/inbox/tour/assets/built-a1.webp"));
}

#[test]
fn sending_is_separate_from_picking_and_commenting() -> Result<()> {
    let mut replay = Replay::default();
    replay.go(Stage::WarmSend);
    assert!(replay.send().is_err());
    replay.warm = Some("Evaluate only".to_owned());
    replay.send()?;
    assert_eq!(replay.stage, Stage::WarmReplay);
    assert!(!replay.delivered);
    replay.go(Stage::Words);
    replay.save_comment(Target::Message, "selected words".to_owned(), "my note".to_owned())?;
    assert_eq!(replay.stage, Stage::FileOpen);
    assert!(replay.comments.iter().all(|comment| !comment.sent));
    replay.save_comment(Target::File, "selected line".to_owned(), "file note".to_owned())?;
    assert_eq!(replay.stage, Stage::Send);
    replay.picked = Some("another direction".to_owned());
    replay.send()?;
    assert_eq!(replay.stage, Stage::Replay);
    assert!(replay.comments.iter().all(|comment| comment.sent));
    Ok(())
}

#[test]
fn image_drawings_and_film_moments_stay_in_local_drafts() -> Result<()> {
    let mut replay = Replay::default();
    replay.circle.x = 0.3;
    replay.save_comment(Target::Image, "a1.png".to_owned(), "here".to_owned())?;
    assert_eq!(replay.comments.first().and_then(|comment| comment.circle).map(|circle| circle.x), Some(0.3));
    replay.moment = 7.2;
    replay.save_comment(Target::Film, "frame".to_owned(), "larger".to_owned())?;
    assert_eq!(replay.comments.last().and_then(|comment| comment.at), Some(7.2));
    replay.send()?;
    assert_eq!(replay.stage, Stage::FilmReplay);
    assert!(replay.comments.iter().any(|comment| comment.target == Target::Image && !comment.sent));
    assert!(replay.comments.iter().any(|comment| comment.target == Target::Film && comment.sent));
    Ok(())
}

#[test]
fn fast_forward_records_skips_without_manufacturing_comments() -> Result<()> {
    let fixture = fixture()?;
    let mut replay = Replay::default();
    replay.go(Stage::Words);
    replay.skip(&fixture)?;
    assert!(replay.skipped.contains(&4));
    assert!(replay.comments.is_empty());
    assert_eq!(replay.stage, Stage::FileOpen);
    Ok(())
}

#[test]
fn new_thread_requires_an_explicit_recipient_and_nonempty_body() -> Result<()> {
    let mut replay = Replay::default();
    replay.go(Stage::NewSend);
    assert!(replay.send().is_err());
    replay.new_body = "my own request".to_owned();
    assert!(replay.send().is_err());
    replay.recipient = true;
    replay.send()?;
    assert_eq!(replay.stage, Stage::NewReplay);
    assert_eq!(replay.new_body, "my own request");
    Ok(())
}
