//! Report submission freezes only validated text; failed delivery keeps its payload.
pub fn submit_reason(draft: &str, retry: Option<&str>) -> Result<String, &'static str> {
    let reason = retry.unwrap_or(draft);
    if !thiscord_shared::safety::valid_report_text(reason) {
        return Err(thiscord_shared::safety::REPORT_TEXT_ERROR);
    }
    Ok(reason.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_reason_remains_correctable_before_freezing() {
        let mut frozen = None;
        for draft in ["", " \t\n", "explanation\0", "explanation\u{7f}"] {
            if let Ok(reason) = submit_reason(draft, frozen.as_deref()) {
                frozen = Some(reason);
            }
            assert!(
                frozen.is_none(),
                "invalid input must not make the editor read-only"
            );
        }
        frozen = Some(submit_reason("Corrected explanation", frozen.as_deref()).unwrap());
        assert_eq!(frozen.as_deref(), Some("Corrected explanation"));
        // A lost response must retry the same UUID/payload, not newer input.
        assert_eq!(
            submit_reason("changed after failed delivery", frozen.as_deref()).unwrap(),
            "Corrected explanation"
        );
    }
    #[test]
    fn report_reason_counts_unicode_and_allows_readable_newlines() {
        assert!(submit_reason(&"界".repeat(2000), None).is_ok());
        assert!(submit_reason(&"界".repeat(2001), None).is_err());
        assert_eq!(submit_reason("One\n\tTwo", None).unwrap(), "One\n\tTwo");
    }
}
