id!(
    /// A job's directory name on the machine, `dibs out <job>` finds it by.
    JobId
);

impl JobId {
    /// `text` as a job's id, only when it is one, so one read from a process's environment names
    /// no other path.
    ///
    /// # Guarantees
    ///
    /// - The id is not empty and holds only ASCII digits and dashes, as a job's directory is named.
    pub fn checked_from_text(text: &str) -> Option<JobId> {
        is_job_id(text).then(|| JobId::new(text))
    }
}

fn is_job_id(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_and_dashes_are_a_job_id() {
        assert_eq!(
            JobId::checked_from_text("20261001120000-42"),
            Some(JobId::new("20261001120000-42"))
        );
    }

    #[test]
    fn empty_text_is_no_job_id() {
        assert_eq!(JobId::checked_from_text(""), None);
    }

    #[test]
    fn a_path_is_no_job_id() {
        assert_eq!(JobId::checked_from_text("../42"), None);
    }
}
