//! Independent local approval. The model never supplies an approval boolean.
use super::agent_read_request::AgentReadRequest;
use bridge_tally_core::TallyDate;
use bridge_tally_protocol::{
    outstandings_shared::DateBoundaryProfile, StandardLedgerCatalogBinding,
};
use std::{io::Read, num::NonZeroUsize, process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

const MAX_PREVIEW_BYTES: usize = 8_000;
#[cfg(not(windows))]
const POST_LABEL: &str = "Post voucher";
/// The review dialog's positive button, as the person sees it. Windows shows
/// a Yes/No/Cancel message box, which has no custom labels.
#[cfg(not(windows))]
pub(crate) const REVIEW_BUTTON: &str = "I reviewed it";
#[cfg(windows)]
pub(crate) const REVIEW_BUTTON: &str = "Yes";
/// What the review dialog's subprocess prints, followed by the nonce it was
/// given, when and only when the person chose the positive button.
const REVIEW_TOKEN_PREFIX: &str = "bridge-review-acknowledged:";
/// The same for the post dialog (#635). Distinct from the review's, so a
/// subprocess in one mode can never answer the other.
const POST_TOKEN_PREFIX: &str = "bridge-post-approved:";

/// How many vouchers a dialog asks about, so that its title and button say so
/// (#746). Never zero: a dialog about no voucher approves nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VoucherCount(NonZeroUsize);

impl VoucherCount {
    pub(crate) fn new(count: usize) -> Option<Self> {
        NonZeroUsize::new(count).map(Self)
    }

    /// The count line the dialog child reads, in the one form the parent
    /// writes: decimal digits, with no sign and no leading zero.
    fn parse(line: &str) -> Option<Self> {
        let count = line.parse::<NonZeroUsize>().ok()?;
        (count.to_string() == line).then_some(Self(count))
    }

    /// `None` for one voucher, whose dialog keeps its single-voucher words,
    /// and the count for a batch.
    fn batch(self) -> Option<NonZeroUsize> {
        (self.0.get() > 1).then_some(self.0)
    }
}

#[derive(Clone)]
pub(crate) struct ApprovedImport {
    xml: String,
    /// Every voucher's date, in batch order: the queue's Education recheck
    /// covers each of them.
    voucher_dates: Vec<TallyDate>,
    verification_request: AgentReadRequest,
    ledger_catalogue_request: AgentReadRequest,
    ledger_binding: StandardLedgerCatalogBinding,
    /// The group collection, for a Payment, Receipt or Contra: its legs'
    /// classification is re-derived from it inside the queue. A Journal has
    /// none, so it adds no group read to the queue.
    group_collection_request: Option<AgentReadRequest>,
    /// The company's Currency masters, re-read inside the queue: a post goes
    /// only into a book with exactly one (bridge#551).
    currency_request: AgentReadRequest,
    /// The all-company change marks, read last before the POST to confirm the
    /// aim and again right after it to see where the voucher went (#574).
    company_marks_request: AgentReadRequest,
}

/// What the queue read for the last admission before the POST.
pub(crate) struct QueuedAdmission<'a> {
    pub(crate) first: &'a str,
    pub(crate) second: &'a str,
    pub(crate) catalogue: &'a str,
    pub(crate) groups: Option<&'a str>,
    pub(crate) currencies: &'a str,
    /// The all-company marks read as the binding reads began (#239).
    pub(crate) company_marks_at_binding: &'a str,
    pub(crate) company_marks: &'a str,
    pub(crate) ledger_binding: &'a StandardLedgerCatalogBinding,
}

/// Whether the profile accepts every voucher's date, and there is at least
/// one: an empty list approves nothing.
fn every_date_accepted(profile: DateBoundaryProfile, voucher_dates: &[TallyDate]) -> bool {
    !voucher_dates.is_empty()
        && voucher_dates
            .iter()
            .all(|voucher_date| profile.accepts_boundary(voucher_date))
}

impl ApprovedImport {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn confirm(
        xml: String,
        preview: &str,
        voucher_dates: Vec<TallyDate>,
        verification_request: AgentReadRequest,
        ledger_catalogue_request: AgentReadRequest,
        ledger_binding: StandardLedgerCatalogBinding,
        group_collection_request: Option<AgentReadRequest>,
        currency_request: AgentReadRequest,
        company_marks_request: AgentReadRequest,
    ) -> Result<Self, String> {
        let count = VoucherCount::new(voucher_dates.len())
            .ok_or_else(|| "voucher_date_invalid".to_string())?;
        approve(count, preview).await?;
        Ok(Self {
            xml,
            voucher_dates,
            verification_request,
            ledger_catalogue_request,
            ledger_binding,
            group_collection_request,
            currency_request,
            company_marks_request,
        })
    }

    pub(super) fn xml(&self) -> &str {
        &self.xml
    }

    pub(super) fn verification_request(&self) -> AgentReadRequest {
        self.verification_request.clone()
    }

    pub(super) fn ledger_catalogue_request(&self) -> AgentReadRequest {
        self.ledger_catalogue_request.clone()
    }

    pub(super) fn ledger_binding(&self) -> &StandardLedgerCatalogBinding {
        &self.ledger_binding
    }

    pub(super) fn group_collection_request(&self) -> Option<AgentReadRequest> {
        self.group_collection_request.clone()
    }

    pub(super) fn currency_request(&self) -> AgentReadRequest {
        self.currency_request.clone()
    }

    pub(super) fn company_marks_request(&self) -> AgentReadRequest {
        self.company_marks_request.clone()
    }

    /// Recheck the operator-approved dates after the endpoint queue admits this
    /// request. The observed product/mode can change while native approval waits.
    pub(super) fn require_boundary_profile(
        &self,
        profile: DateBoundaryProfile,
    ) -> Result<(), ApprovedImportAdmissionError> {
        if every_date_accepted(profile, &self.voucher_dates) {
            Ok(())
        } else {
            Err(ApprovedImportAdmissionError::EducationVoucherDateUnsupported)
        }
    }

    #[cfg(test)]
    pub(super) fn approved_for_test(
        xml: String,
        voucher_date: TallyDate,
        ledger_catalogue_request: AgentReadRequest,
        ledger_binding: StandardLedgerCatalogBinding,
        currency_request: AgentReadRequest,
        company_marks_request: AgentReadRequest,
    ) -> Self {
        // Carries the seam marker so the shipped-binary scan also covers this
        // bypass (bridge#583).
        std::hint::black_box(test_seam::SEAM_MARKER);
        Self {
            xml,
            voucher_dates: vec![voucher_date],
            verification_request: AgentReadRequest::parse(
                bridge_tally_protocol::xml_read_profiles::ReadOnlyProfile::CompanyListV2.render(),
            )
            .expect("static read profile is admitted"),
            ledger_catalogue_request,
            ledger_binding,
            group_collection_request: None,
            currency_request,
            company_marks_request,
        }
    }
}

/// How long a post dialog stays open before it approves nothing: the limit
/// [`nonce_bound_dialog`] applies. Used only to tell a caller how much of it
/// remains; the dialog enforces its own.
const POST_DIALOG_LIMIT: Duration = Duration::from_secs(120);

/// When a post dialog was answered, and whether the answer approved: stamped
/// by the dialog's own task as it ends, so an approval's age runs from the
/// click, not from whichever call later collects it (#725). Both clocks are
/// kept: the monotonic one does not advance while the machine sleeps.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Answered {
    pub(crate) approved: bool,
    pub(crate) at: std::time::Instant,
    pub(crate) at_wall: std::time::SystemTime,
}

impl Answered {
    fn now(approved: bool) -> Self {
        Self {
            approved,
            at: std::time::Instant::now(),
            at_wall: std::time::SystemTime::now(),
        }
    }
}

/// A post dialog left open after the MCP call that asked it returned (#725).
/// It is [`ApprovedImport::confirm`] itself, unchanged, run on its own task so
/// that the dialog's time limit and its child's exit are observed while no call
/// is waiting. Dropping this aborts that task, which drops the dialog child:
/// `kill_on_drop` closes the dialog, and a token it prints afterwards is never
/// read, so a late click approves nothing.
pub(crate) struct PendingPostApproval {
    task: tokio::task::JoinHandle<Result<ApprovedImport, String>>,
    /// The answer and, for a refusal, its code: one stamp, set once.
    answered: std::sync::Arc<std::sync::OnceLock<(Answered, Option<String>)>>,
    started: std::time::Instant,
}

impl PendingPostApproval {
    /// Ask the person in a dialog that may outlive the calling call:
    /// [`ApprovedImport::confirm`] with exactly these arguments, on its own
    /// task. Nothing else can be run there.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ask(
        xml: String,
        preview: String,
        voucher_dates: Vec<TallyDate>,
        verification_request: AgentReadRequest,
        ledger_catalogue_request: AgentReadRequest,
        ledger_binding: StandardLedgerCatalogBinding,
        group_collection_request: Option<AgentReadRequest>,
        currency_request: AgentReadRequest,
        company_marks_request: AgentReadRequest,
    ) -> Self {
        let answered = std::sync::Arc::new(std::sync::OnceLock::new());
        let stamp = std::sync::Arc::clone(&answered);
        let dialog = async move {
            let answer = ApprovedImport::confirm(
                xml,
                &preview,
                voucher_dates,
                verification_request,
                ledger_catalogue_request,
                ledger_binding,
                group_collection_request,
                currency_request,
                company_marks_request,
            )
            .await;
            // Stamped before the task ends, never after: a task seen finished
            // with no stamp is one that ended without an answer. A refusal's
            // code is stamped with it, so it can be read without the task.
            let _ = stamp.set((
                Answered::now(answer.is_ok()),
                answer.as_ref().err().cloned(),
            ));
            answer
        };
        Self {
            task: tokio::spawn(carry_approval_scope(dialog)),
            answered,
            started: std::time::Instant::now(),
        }
    }

    /// The person's answer and when it was given, if it arrives within
    /// `budget`, or the dialog back while it is still open. A task that ended
    /// without an answer (aborted or panicked) approves nothing.
    pub(crate) async fn answer_within(
        mut self,
        budget: Duration,
    ) -> Result<(Result<ApprovedImport, String>, Answered), Self> {
        match tokio::time::timeout(budget, &mut self.task).await {
            Err(_) => Err(self),
            Ok(Ok(answer)) => {
                let at = self
                    .answered()
                    .unwrap_or_else(|| Answered::now(answer.is_ok()));
                Ok((answer, at))
            }
            Ok(Err(_)) => Ok((
                Err("import_approval_unavailable".into()),
                Answered::now(false),
            )),
        }
    }

    /// Whether the dialog has been answered, and how, without waiting.
    pub(crate) fn answered(&self) -> Option<Answered> {
        self.answered.get().map(|(answered, _)| *answered)
    }

    /// Why the dialog approved nothing, once it has ended without an approval:
    /// the stamped refusal code, or `import_approval_unavailable` for a task
    /// that ended with no answer, as [`Self::answer_within`] reports it.
    /// `None` while the dialog is open, and for an approval.
    pub(crate) fn refusal(&self) -> Option<String> {
        // Finished is read before the stamp: the stamp is set before the task
        // ends, so a task seen finished with no stamp truly ended unanswered.
        // Read the other way, a dialog stamped and finished between the two
        // reads would drop a real approval as unavailable.
        let finished = self.task.is_finished();
        match self.answered.get() {
            Some((_, refusal)) => refusal.clone(),
            None if finished => Some("import_approval_unavailable".into()),
            None => None,
        }
    }

    /// How much of the dialog's time limit remains.
    pub(crate) fn remaining(&self) -> Duration {
        POST_DIALOG_LIMIT.saturating_sub(self.started.elapsed())
    }
}

impl Drop for PendingPostApproval {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ApprovedImport {
    /// How many vouchers the person was asked about: the dialog's count is
    /// taken from these dates (#746), so a redeemed approval can be held to the
    /// batch it is redeemed for.
    pub(crate) fn voucher_count(&self) -> usize {
        self.voucher_dates.len()
    }
}

/// A person's answer to the review dialog for a doubted post (#239): that
/// they checked the voucher in Tally. It changes nothing in Tally and
/// authorises no post: it is a different type from [`ApprovedImport`], built
/// only by [`ReviewAcknowledged::confirm`], and nothing converts one into the
/// other.
#[must_use]
pub(crate) struct ReviewAcknowledged(());

impl ReviewAcknowledged {
    pub(crate) async fn confirm(count: VoucherCount, preview: &str) -> Result<Self, String> {
        approve_review(count, preview).await?;
        Ok(Self(()))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ApprovedImportAdmissionError {
    #[error("education_voucher_date_unsupported")]
    EducationVoucherDateUnsupported,
    #[error("import_preexisting_identity")]
    PreexistingIdentity,
    #[error("import_masters_changed")]
    LedgerIdentityChanged,
    /// A Payment, Receipt or Contra leg no longer classifies as it did when
    /// approved: a ledger or one of its groups moved (bridge#466 follow-up).
    #[error("import_bank_classification_changed")]
    BankClassificationChanged,
    /// A ledger a bank cash answer named as cash in hand no longer reaches
    /// Cash-in-Hand: it or one of its groups moved since the build (#815).
    /// Carries each refused ledger's row, as the build's
    /// `cash_ledger_not_cash_in_hand` refusal reports it, and how many rows the
    /// recheck's budget left out.
    #[error("cash_ledger_not_cash_in_hand")]
    CashLedgerNotCashInHand {
        refused: Vec<serde_json::Value>,
        omitted: usize,
    },
    /// The batch was recorded before Bridge stored the ledgers its bank cash
    /// answers named as cash in hand, so there is nothing to check (#815).
    #[error("import_batch_predates_cash_ledger_record")]
    CashLedgersNotRecorded,
    /// The batch was recorded before Bridge stored which bill-wise ledgers a
    /// person approved to receive entries On Account, so there is nothing to
    /// check (#1234).
    #[error("import_batch_predates_bill_wise_record")]
    BillWiseNotRecorded,
    /// A ledger the batch names is bill-wise now and was not approved: it was
    /// switched to bill-wise after the build, so an entry on it would land On
    /// Account unseen (#1234).
    #[error("import_bill_wise_changed")]
    BillWiseChanged,
    /// A named ledger now folds equal to another live ledger, which Tally's
    /// import lookup could take for it (bridge#626).
    #[error("ledger_has_folded_twin")]
    LedgerFoldedTwin,
    /// The group collection the queue re-read for a bank voucher's
    /// classification could not be parsed. Carries the snapshot parser's own
    /// data-free code, as the read before approval does (bridge#717).
    #[error("group_export_invalid")]
    GroupExportInvalid { cause: Option<&'static str> },
    /// A bank voucher reached the queue without its group read, or a Journal
    /// with one: a wiring fault, refused before any request is sent.
    #[error("import_post_admission_inconsistent")]
    AdmissionInconsistent,
    /// The snapshot sent last before the POST no longer shows exactly one
    /// loaded company with the target's GUID and name, or shows another loaded
    /// company sharing its name (#574).
    #[error("post_company_scope_changed")]
    CompanyScopeChanged,
    /// That snapshot could not be read, so the aim cannot be confirmed.
    #[error("post_company_scope_unconfirmed")]
    CompanyScopeUnconfirmed,
    /// The company defines more than one Currency master. Bridge's amounts are
    /// plain base-currency figures, and the write path does not compare a
    /// leg's currency with an identified base yet, so no leg can be shown to
    /// be in it (bridge#551). Carries every master's NAME, for the refusal to name.
    #[error("import_multi_currency_unsupported")]
    MultiCurrencyBook { currencies: Vec<String> },
    /// The company's Currency masters read as none, or the response does not
    /// parse (a master without a NAME does not).
    #[error("import_base_currency_undetermined")]
    BaseCurrencyUndetermined,
    /// The target's master mark (ALTMSTID) moved between the snapshot taken as
    /// the queue's binding reads began and the aim snapshot read last before
    /// the POST: a master changed after the catalogue re-read (bridge#239).
    #[error("post_masters_moved")]
    MastersMoved,
    /// That comparison could not be made: the first snapshot could not be
    /// read, or either did not hold exactly one row for the target.
    #[error("post_masters_unconfirmed")]
    MastersUnconfirmed,
    /// The ledger catalogue the queue re-read could not be parsed, so the
    /// approved binding cannot be rechecked. Raised only by that recheck,
    /// before the intent or the POST (bridge#641). The source is the typed,
    /// data-free cause the refusal carries.
    #[error("post_catalogue_unreadable")]
    CatalogueUnreadable(#[source] bridge_tally_protocol::StandardLedgerCatalogError),
}

/// The data-free code a group snapshot refusal carries as its `cause`, for
/// the read before approval and the queue's re-read alike (bridge#676, #717).
pub(crate) fn group_snapshot_cause(
    error: &bridge_tally_protocol::native_outstandings::NativeOutstandingsError,
) -> Option<&'static str> {
    use bridge_tally_protocol::native_outstandings::NativeOutstandingsError;
    match error {
        NativeOutstandingsError::InvalidResponse(code) => Some(code),
        NativeOutstandingsError::TallyReportedFailure => Some("group_status_not_success"),
        NativeOutstandingsError::StatusAbsent => Some("group_status_absent"),
        _ => None,
    }
}

/// A failure inside the endpoint queue before the dispatch intent is recorded
/// (#656): every queue read, and the admission recheck, run in one block whose
/// error this wraps; the intent, the POST and the readback run after it. So it
/// marks a refusal whose outcome is known — nothing was sent — by where it
/// happened, and a read added to that block later is covered without a list.
/// A named admission refusal inside it keeps its own code.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub(crate) struct PreIntentQueueRefusal {
    #[source]
    pub(crate) source: anyhow::Error,
}

/// A refusal under the exclusive admission lock, just before the dispatch
/// intent is appended (#711). Nothing was recorded and nothing was sent, so
/// each keeps its own code instead of the catch-all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UnderLockRefusal {
    #[error("import_batch_not_found")]
    BatchNotFound,
    #[error("import_already_attempted")]
    AlreadyAttempted,
    #[error("import_remote_id_reused")]
    RemoteIdReused,
    #[error("import_batch_changed")]
    BatchChanged,
    #[error("import_txn_already_posted")]
    TxnAlreadyPosted,
    /// The aim snapshot did not yield the target's voucher mark to record with
    /// the intent. The aim check before it requires exactly one target row, so
    /// this should not fire; if it does, no dispatch intent is recorded and
    /// nothing is sent.
    #[error("post_mark_unrecorded")]
    MarkUnrecorded,
    /// The approval could not be spent: it was revoked after this call took
    /// it, or a voucher post reached the lock without one (#791).
    #[error("import_approval_revoked")]
    ApprovalRevoked,
}

impl UnderLockRefusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::BatchNotFound => "import_batch_not_found",
            Self::AlreadyAttempted => "import_already_attempted",
            Self::RemoteIdReused => "import_remote_id_reused",
            Self::BatchChanged => "import_batch_changed",
            Self::TxnAlreadyPosted => "import_txn_already_posted",
            Self::MarkUnrecorded => "post_mark_unrecorded",
            Self::ApprovalRevoked => "import_approval_revoked",
        }
    }
}

/// Why `before_dispatch` refused. Only a named check made before the intent
/// append is `Refused`; the lock, the journal read and the append itself are
/// `Other`, which keeps the catch-all because the append may have recorded an
/// intent (#711).
/// Built explicitly at each site: this file holds no conversion (the approval
/// seam gate refuses any `impl From`).
#[derive(Debug)]
pub(crate) enum BeforeDispatchError {
    Refused(UnderLockRefusal),
    Other(String),
}

/// The native approval every real post goes through. Outside this crate's own
/// unit tests it is exactly [`confirm`]: nothing else exists to answer it.
#[cfg(not(test))]
use confirm as approve;

#[cfg(test)]
use test_seam::approve;

/// The native review dialog every acknowledgement goes through, gated exactly
/// as [`approve`] is.
#[cfg(not(test))]
use confirm_review as approve_review;

#[cfg(test)]
use test_seam::approve_review;

/// What a post dialog's own task runs under (#725). Outside this crate's unit
/// tests it is the dialog alone: nothing is carried into the task.
#[cfg(not(test))]
use carry_nothing as carry_approval_scope;

#[cfg(test)]
use test_seam::carry_approval_scope;

#[cfg(not(test))]
fn carry_nothing<F>(dialog: F) -> F {
    dialog
}

/// A scripted answer to the native approval, for this crate's unit tests only
/// (bridge#583). It is compiled only under bare `cfg(test)`, which Cargo sets
/// for no shipped build and no feature, variable or flag can set at runtime;
/// `tests/approval_seam_gate.rs` holds it to that, and
/// `scripts/check-no-test-seam.mjs` proves its marker is absent from every
/// shipped executable.
#[cfg(test)]
pub(crate) mod test_seam {
    use std::sync::{Arc, Mutex};

    /// Present in any binary this module is compiled into, and in no other.
    pub(crate) const SEAM_MARKER: &str = "bridge-test-approval-seam-5f1c9e7a";

    const ONE: super::VoucherCount = super::VoucherCount(std::num::NonZeroUsize::MIN);

    fn count(count: usize) -> super::VoucherCount {
        super::VoucherCount::new(count).unwrap()
    }

    /// What a test decided, and every preview the post path asked it about.
    #[derive(Clone)]
    pub(crate) struct ScriptedApproval {
        approve: bool,
        previews: Arc<Mutex<Vec<String>>>,
        /// Every preview the review dialog was asked about, kept apart from
        /// the post dialog's so a test can tell which dialog a person saw.
        reviews: Arc<Mutex<Vec<String>>>,
        /// The voucher count each dialog was asked about, in the same order
        /// as `previews` and `reviews` (#746).
        counts: Arc<Mutex<Vec<usize>>>,
        review_counts: Arc<Mutex<Vec<usize>>>,
        /// Run while the approval is pending, as something else changing the
        /// book or the journal while an operator reads the dialog would.
        while_pending: Option<Arc<dyn Fn() + Send + Sync>>,
        /// When set, the post dialog stays open until a test answers it
        /// through [`ScriptedApproval::answer`], as a person who has not yet
        /// clicked would (#725). `approve` is then ignored.
        held: Option<Arc<tokio::sync::watch::Sender<Option<bool>>>>,
        /// Signalled when a held post dialog opens, so a test waits for that
        /// event rather than polling for it against a clock.
        opened: Arc<tokio::sync::Notify>,
    }

    impl ScriptedApproval {
        pub(crate) fn approving() -> Self {
            Self::new(true)
        }

        pub(crate) fn declining() -> Self {
            Self::new(false)
        }

        /// Approves, after running `while_pending` as the dialog would wait.
        pub(crate) fn approving_after(while_pending: impl Fn() + Send + Sync + 'static) -> Self {
            Self {
                while_pending: Some(Arc::new(while_pending)),
                ..Self::new(true)
            }
        }

        /// A post dialog that stays open until [`ScriptedApproval::answer`].
        pub(crate) fn held() -> Self {
            Self {
                held: Some(Arc::new(tokio::sync::watch::channel(None).0)),
                ..Self::new(false)
            }
        }

        /// Whether a held post dialog is still open: its task is waiting for
        /// an answer. False once the task was aborted, which closes it.
        pub(crate) fn is_waiting(&self) -> bool {
            self.held
                .as_ref()
                .is_some_and(|held| held.receiver_count() > 0)
        }

        /// Resolves once a held post dialog has opened: its task is waiting
        /// for an answer. A permit is kept, so it also resolves when the
        /// dialog opened before this was awaited.
        pub(crate) async fn opened(&self) {
            self.opened.notified().await;
        }

        /// Resolves once an opened held post dialog has closed: its task took
        /// an answer or was aborted. Await it only after [`Self::opened`]; a
        /// dialog not yet open counts as closed.
        pub(crate) async fn closed(&self) {
            if let Some(held) = &self.held {
                held.closed().await;
            }
        }

        /// Answer a held post dialog. Answering one that was closed (its task
        /// aborted) reaches nothing.
        pub(crate) fn answer(&self, approve: bool) {
            if let Some(held) = &self.held {
                held.send_replace(Some(approve));
            }
        }

        fn new(approve: bool) -> Self {
            Self {
                approve,
                previews: Arc::default(),
                reviews: Arc::default(),
                counts: Arc::default(),
                review_counts: Arc::default(),
                while_pending: None,
                held: None,
                opened: Arc::default(),
            }
        }

        pub(crate) fn previews(&self) -> Vec<String> {
            self.previews.lock().unwrap().clone()
        }

        pub(crate) fn reviews(&self) -> Vec<String> {
            self.reviews.lock().unwrap().clone()
        }

        pub(crate) fn counts(&self) -> Vec<usize> {
            self.counts.lock().unwrap().clone()
        }

        pub(crate) fn review_counts(&self) -> Vec<usize> {
            self.review_counts.lock().unwrap().clone()
        }
    }

    tokio::task_local! {
        /// The decision for the one task a test scopes it to. A task-local
        /// does not cross `tokio::spawn`: an approval asked from a spawned
        /// task finds no decision and is declined, which fails safe.
        pub(crate) static SCRIPTED_APPROVAL: ScriptedApproval;
    }

    /// The test-build approval. Unscoped, it declines at once and starts no
    /// process, so no test can reach a real dialog or approve by default.
    pub(super) async fn approve(count: super::VoucherCount, preview: &str) -> Result<(), String> {
        let decision = SCRIPTED_APPROVAL
            .try_with(|scripted| {
                scripted.previews.lock().unwrap().push(preview.to_string());
                scripted.counts.lock().unwrap().push(count.0.get());
                if let Some(while_pending) = &scripted.while_pending {
                    while_pending();
                }
                match &scripted.held {
                    Some(held) => {
                        let waiting = held.subscribe();
                        scripted.opened.notify_one();
                        Err(waiting)
                    }
                    None => Ok(scripted.approve),
                }
            })
            .unwrap_or(Ok(false));
        let decision = match decision {
            Ok(decision) => decision,
            Err(mut held) => {
                let answered = held
                    .wait_for(Option::is_some)
                    .await
                    .map(|answer| *answer == Some(true))
                    .unwrap_or(false);
                answered
            }
        };
        std::hint::black_box(SEAM_MARKER);
        if decision {
            Ok(())
        } else {
            Err("import_approval_declined".into())
        }
    }

    /// Carries the test's scripted decision into a post dialog's own task
    /// (#725). A task-local does not cross `tokio::spawn`, so without this a
    /// dialog asked from its task would find no decision and decline; with it,
    /// the task sees exactly the decision the asking test scoped, and an
    /// unscoped test still declines.
    pub(crate) fn carry_approval_scope<F>(
        dialog: F,
    ) -> impl std::future::Future<Output = F::Output> + Send + 'static
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send,
    {
        let scripted = SCRIPTED_APPROVAL.try_with(Clone::clone).ok();
        async move {
            match scripted {
                Some(scripted) => SCRIPTED_APPROVAL.scope(scripted, dialog).await,
                None => dialog.await,
            }
        }
    }

    /// The test-build review dialog, scripted by the same decision and
    /// declining the same way when unscoped.
    pub(super) async fn approve_review(
        count: super::VoucherCount,
        preview: &str,
    ) -> Result<(), String> {
        let decision = SCRIPTED_APPROVAL
            .try_with(|scripted| {
                scripted.reviews.lock().unwrap().push(preview.to_string());
                scripted.review_counts.lock().unwrap().push(count.0.get());
                if let Some(while_pending) = &scripted.while_pending {
                    while_pending();
                }
                scripted.approve
            })
            .unwrap_or(false);
        std::hint::black_box(SEAM_MARKER);
        if decision {
            Ok(())
        } else {
            Err("ack_review_declined".into())
        }
    }

    /// The real approval keeps its own preview limit; the scripted one does
    /// not repeat it, so the limit is held here, on the real path, where it
    /// refuses before any process is started.
    #[tokio::test]
    async fn the_real_approval_refuses_an_oversized_preview_before_starting_a_process() {
        let oversized = "x".repeat(super::MAX_PREVIEW_BYTES + 1);
        assert_eq!(
            super::confirm(ONE, &oversized).await,
            Err("import_review_too_large".to_string())
        );
    }

    /// The same for the review dialog (#239), which has its own limit code.
    #[tokio::test]
    async fn the_real_review_refuses_an_oversized_preview_before_starting_a_process() {
        let oversized = "x".repeat(super::MAX_PREVIEW_BYTES + 1);
        assert_eq!(
            super::confirm_review(ONE, &oversized).await,
            Err("ack_review_too_large".to_string())
        );
    }

    /// The dialog child's stand-in, `examples/approval_standin.rs` (#702),
    /// built beside this test executable: `cargo test` and `cargo nextest
    /// run` build examples, `cargo test --lib` does not. Missing, it fails the
    /// test; it never skips it.
    fn standin_example() -> std::path::PathBuf {
        let test = std::env::current_exe().unwrap();
        let profile = test.parent().and_then(std::path::Path::parent).unwrap();
        let example = profile
            .join("examples")
            .join(format!("approval_standin{}", std::env::consts::EXE_SUFFIX));
        assert!(
            example.is_file(),
            "{} is missing: build the approval_standin example (cargo test without --lib)",
            example.display()
        );
        example
    }

    /// A directory for stand-ins, beside the example on the same volume, so
    /// [`standin`] can link to it.
    fn standin_directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("approval-standin-")
            .tempdir_in(standin_example().parent().unwrap())
            .unwrap()
    }

    /// A stand-in for a dialog subprocess that writes `answer`, with
    /// `{NONCE}` replaced by the nonce it was sent, and exits with `exit_code`.
    /// Its first act is to create `<stand-in>.ran`, which [`standin_input`]
    /// reads, so a row can tell a refusal the stand-in produced from a spawn
    /// that failed; before answering, it saves the input it read there.
    ///
    /// Each call is a hard link to the built example, never a copy. A link
    /// opens no descriptor, so the test process never holds a writable one to
    /// an executable. If it did, another test thread's fork would inherit that
    /// descriptor until its exec, and exec'ing the stand-in in that window
    /// fails on Linux with "text file busy" (ETXTBSY). That failure surfaces
    /// as `…_unavailable`, the very code some rows expect (#704 review).
    fn standin(directory: &std::path::Path, exit_code: i32, answer: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = directory.join(format!(
            "row-{}{}",
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::hard_link(standin_example(), &path).unwrap();
        std::fs::write(beside(&path, ".answer"), format!("{exit_code}\n{answer}")).unwrap();
        path
    }

    fn beside(path: &std::path::Path, extension: &str) -> std::path::PathBuf {
        let mut path = path.as_os_str().to_owned();
        path.push(extension);
        path.into()
    }

    /// What the stand-in at `standin` was last sent, or `None` if it never
    /// started: see [`standin`].
    fn standin_input(standin: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(beside(standin, ".ran")).ok()
    }

    /// The control for [`standin_input`]. A stand-in that cannot start is
    /// refused as unavailable too, and leaves no input behind. So each row's
    /// input check is what tells a refusal the stand-in produced from a spawn
    /// that failed.
    #[tokio::test]
    async fn a_stand_in_that_cannot_start_is_unavailable_and_leaves_no_marker() {
        let directory = standin_directory();
        let path = directory
            .path()
            .join(format!("not-executable{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, "not an executable image\n").unwrap();
        assert_eq!(
            super::confirm_with(&path, ONE, "Post").await,
            Err("import_approval_unavailable".to_string())
        );
        assert_eq!(standin_input(&path), None);
    }

    /// Only the token echoing this call's nonce is an answer, and it is
    /// accepted whatever the exit status. A person's decline is no token and
    /// exit 1. A clean exit without the token is never that (#689): it is an
    /// executable that is not this dialog, so it is refused as unavailable:
    /// an older build that ignores `--confirm-review`, a process that echoes
    /// its input, a token for another nonce, the post dialog's token, or the
    /// token with stray output, with CRLF or without its newline.
    #[tokio::test]
    async fn the_review_is_answered_only_by_the_token_for_its_nonce() {
        let directory = standin_directory();
        let answers = [
            (
                "a person's decline: no token, exit 1",
                1,
                "",
                Err("ack_review_declined"),
            ),
            (
                "an older build exits 0",
                0,
                "",
                Err("ack_review_unavailable"),
            ),
            (
                "an echo of the input",
                0,
                "{NONCE}\n1\nReview",
                Err("ack_review_unavailable"),
            ),
            (
                "a token for another nonce",
                0,
                "bridge-review-acknowledged:00000000-0000-4000-8000-000000000000\n",
                Err("ack_review_unavailable"),
            ),
            (
                "the post dialog's token for this nonce",
                0,
                "bridge-post-approved:{NONCE}\n",
                Err("ack_review_unavailable"),
            ),
            (
                "a log line, then the token",
                0,
                "starting\nbridge-review-acknowledged:{NONCE}\n",
                Err("ack_review_unavailable"),
            ),
            (
                "the token with CRLF",
                0,
                "bridge-review-acknowledged:{NONCE}\r\n",
                Err("ack_review_unavailable"),
            ),
            (
                "the token without its newline",
                0,
                "bridge-review-acknowledged:{NONCE}",
                Err("ack_review_unavailable"),
            ),
            (
                "the token, but a failing exit",
                1,
                "bridge-review-acknowledged:{NONCE}\n",
                Ok(()),
            ),
        ];
        for (name, exit_code, answer, expected) in answers {
            // The stand-in must have run: a spawn failure is also
            // `ack_review_unavailable`, and would pass a row without reaching
            // the clean-exit-without-token arm it is here to pin.
            let path = standin(directory.path(), exit_code, answer);
            let result = super::confirm_review_with(&path, ONE, "Review").await;
            assert_eq!(result, expected.map_err(str::to_string), "{name}");
            assert!(standin_input(&path).is_some(), "{name}: the stand-in ran");
        }
        // The control: the token for this call's nonce is accepted.
        let echoes_token = standin(directory.path(), 0, "bridge-review-acknowledged:{NONCE}\n");
        assert_eq!(
            super::confirm_review_with(&echoes_token, ONE, "Review").await,
            Ok(())
        );
        assert!(standin_input(&echoes_token).is_some());
    }

    /// The post dialog is answered only by the token echoing this call's
    /// nonce, and a clean exit (#635). Anything else is refused, never
    /// approved. A clean exit without the token cannot be a person's decline,
    /// which exits 1, so it is refused as the dialog being unavailable: an
    /// executable that ignores `--confirm-journal`, one that echoes its input,
    /// a token for another nonce, the review dialog's token, or stray output.
    /// A failing exit is a decline, even after the right token.
    #[tokio::test]
    async fn a_post_is_approved_only_by_the_token_for_its_nonce() {
        let directory = standin_directory();
        for (name, exit_code, answer) in [
            ("a person's decline: no token, exit 1", 1, ""),
            (
                "the token, but a failing exit",
                1,
                "bridge-post-approved:{NONCE}\n",
            ),
        ] {
            let path = standin(directory.path(), exit_code, answer);
            assert_eq!(
                super::confirm_with(&path, ONE, "Post").await,
                Err("import_approval_declined".to_string()),
                "{name}"
            );
            assert!(standin_input(&path).is_some(), "{name}: the stand-in ran");
        }
        for (name, answer) in [
            ("an executable ignoring the flag exits 0", ""),
            ("an echo of the input", "{NONCE}\n1\nPost"),
            (
                "a token for another nonce",
                "bridge-post-approved:00000000-0000-4000-8000-000000000000\n",
            ),
            (
                "the review dialog's token for this nonce",
                "bridge-review-acknowledged:{NONCE}\n",
            ),
            // The answer is matched byte for byte, so any stray output, such
            // as a log line, is refused. That is fail-closed on purpose: do
            // not trim or search the output to "fix" it.
            (
                "a log line, then the token",
                "starting\nbridge-post-approved:{NONCE}\n",
            ),
            (
                "the token, then more output",
                "bridge-post-approved:{NONCE}\nmore\n",
            ),
            // What a Windows `echo` writes (#702).
            ("the token with CRLF", "bridge-post-approved:{NONCE}\r\n"),
            (
                "the token without its newline",
                "bridge-post-approved:{NONCE}",
            ),
        ] {
            // The stand-in must have run: a spawn failure is also
            // `import_approval_unavailable`, and would pass this row without
            // reaching the clean-exit-without-token arm it is here to pin.
            let path = standin(directory.path(), 0, answer);
            assert_eq!(
                super::confirm_with(&path, ONE, "Post").await,
                Err("import_approval_unavailable".to_string()),
                "{name}"
            );
            assert!(standin_input(&path).is_some(), "{name}: the stand-in ran");
        }
        // The control: the token for this call's nonce, then a clean exit.
        let approves = standin(directory.path(), 0, "bridge-post-approved:{NONCE}\n");
        assert_eq!(super::confirm_with(&approves, ONE, "Post").await, Ok(()));
        assert!(standin_input(&approves).is_some());
    }

    /// Each call sends a nonce of its own: a stand-in that answers every call
    /// with the token for the nonce it read sees a different one each time.
    #[tokio::test]
    async fn each_post_dialog_gets_a_fresh_nonce() {
        let directory = standin_directory();
        let approves = standin(directory.path(), 0, "bridge-post-approved:{NONCE}\n");
        let mut nonces = Vec::new();
        for _ in 0..2 {
            assert_eq!(super::confirm_with(&approves, ONE, "Post").await, Ok(()));
            let input = standin_input(&approves).expect("the stand-in ran");
            nonces.push(input.lines().next().unwrap().to_string());
        }
        assert!(nonces
            .iter()
            .all(|nonce| uuid::Uuid::parse_str(nonce).is_ok()));
        assert_ne!(nonces[0], nonces[1]);
    }

    /// Each dialog child is told the voucher count its title and button
    /// name, on the line after the nonce, and then the preview (#746). The
    /// stand-in saves the bytes the parent sent, and the child's own parser
    /// reads them, so the writer and the parser are tested together.
    #[tokio::test]
    async fn each_dialog_child_is_told_the_voucher_count() {
        let directory = standin_directory();
        for (prefix, review, sent, preview) in [
            ("bridge-post-approved:", false, 200, "Post"),
            ("bridge-review-acknowledged:", true, 7, "Review"),
        ] {
            let answers = standin(directory.path(), 0, &format!("{prefix}{{NONCE}}\n"));
            let result = if review {
                super::confirm_review_with(&answers, count(sent), preview).await
            } else {
                super::confirm_with(&answers, count(sent), preview).await
            };
            assert_eq!(result, Ok(()), "{prefix}");
            let input = standin_input(&answers).expect("the stand-in ran");
            let (_, shown, text) = super::dialog_input(&input).expect("the parent's shape");
            assert_eq!((shown, text), (count(sent), preview), "{prefix}");
        }
    }

    /// One voucher keeps the single-voucher words. A batch names its count in
    /// each title and on the post button, which never reads "Cancel", the
    /// label that carries the decline (#746).
    #[cfg(not(windows))]
    #[test]
    fn each_dialog_names_a_batch_by_its_count() {
        assert_eq!(
            super::post_words(ONE),
            (
                "ComplyEaze Bridge — approve one voucher".to_string(),
                "Post voucher".to_string()
            )
        );
        assert_eq!(
            super::post_words(count(200)),
            (
                "ComplyEaze Bridge — approve 200 vouchers".to_string(),
                "Post 200 vouchers".to_string()
            )
        );
        assert_eq!(super::post_words(count(2)).1, "Post 2 vouchers");
        assert_eq!(
            super::review_title(ONE),
            "ComplyEaze Bridge — record that you reviewed one voucher"
        );
        assert_eq!(
            super::review_title(count(50)),
            "ComplyEaze Bridge — record that you reviewed 50 vouchers"
        );
    }

    /// The same for the Windows titles, which carry the question.
    #[cfg(windows)]
    #[test]
    fn each_windows_dialog_names_a_batch_by_its_count() {
        assert_eq!(
            super::post_question(ONE),
            "ComplyEaze Bridge — post this voucher?"
        );
        assert_eq!(
            super::post_question(count(200)),
            "ComplyEaze Bridge — post 200 vouchers?"
        );
        assert_eq!(
            super::review_question(ONE),
            "ComplyEaze Bridge — record that you reviewed this voucher?"
        );
        assert_eq!(
            super::review_question(count(50)),
            "ComplyEaze Bridge — record that you reviewed these 50 vouchers?"
        );
    }

    /// The dialog subprocesses show a dialog only for input of the shape the
    /// parent sends: a nonce line, a count line in the one form the parent
    /// writes, then a preview within the limit.
    #[test]
    fn a_dialog_subprocess_admits_only_the_parents_input_shape() {
        let nonce = "9c8d8de4-c06c-447b-8309-60ba702bf663";
        assert_eq!(
            super::dialog_input(&format!("{nonce}\n1\nReview")),
            Some((nonce, ONE, "Review"))
        );
        assert_eq!(
            super::dialog_input(&format!("{nonce}\n200\nPost\nmore")),
            Some((nonce, count(200), "Post\nmore"))
        );
        let oversized = "x".repeat(super::MAX_PREVIEW_BYTES + 1);
        for input in [
            "Review".to_string(),
            "not-a-nonce\n1\nReview".to_string(),
            format!("{nonce}\n1\n"),
            format!("{nonce}\n1\nRe\0view"),
            format!("{nonce}\n1\n{oversized}"),
            // The count line absent, zero, signed, padded, empty, too large,
            // or not a number.
            format!("{nonce}\nReview"),
            format!("{nonce}\n0\nReview"),
            format!("{nonce}\n+2\nReview"),
            format!("{nonce}\n02\nReview"),
            format!("{nonce}\n 2\nReview"),
            format!("{nonce}\n2 \nReview"),
            format!("{nonce}\n\nReview"),
            format!("{nonce}\n99999999999999999999999\nReview"),
            format!("{nonce}\ntwo\nReview"),
        ] {
            assert_eq!(super::dialog_input(&input), None, "{input:.60}");
        }
    }
}

/// The post dialog. An exit status alone is not an answer (#635): an
/// executable that does not know `--confirm-journal`, such as a build of
/// `bridge_mcp` older than the flag left at `current_exe()`, starts the MCP
/// server instead, reads the preview as input and exits 0 at its end. So the
/// parent sends a fresh nonce and requires the token that echoes it, which
/// only this dialog's positive button prints, and a clean exit as well.
async fn confirm(count: VoucherCount, preview: &str) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|_| "import_approval_unavailable")?;
    confirm_with(&executable, count, preview).await
}

async fn confirm_with(
    executable: &std::path::Path,
    count: VoucherCount,
    preview: &str,
) -> Result<(), String> {
    if preview.len() > MAX_PREVIEW_BYTES {
        return Err("import_review_too_large".into());
    }
    match nonce_bound_dialog(
        executable,
        "--confirm-journal",
        POST_TOKEN_PREFIX,
        count,
        preview,
    )
    .await
    {
        Ok(answer) if answer.exited_cleanly => Ok(()),
        // A person's decline is no token and exit 1: `run_confirmation`
        // returns false. A clean exit without the token is never that; it is
        // an executable that does not answer with this token, such as one
        // ignoring the flag, or a build from before #635 whose dialog ran.
        Ok(answer) if answer.exited_cleanly => Err("import_approval_unavailable".into()),
        Ok(_) => Err("import_approval_declined".into()),
        Err(DialogFailure::Unavailable) => Err("import_approval_unavailable".into()),
        Err(DialogFailure::TimedOut) => Err("import_approval_timed_out".into()),
    }
}

/// The review dialog for a doubted post (#239): its own subprocess mode, so
/// its title and button never read as approving a post. It is answered by
/// the token alone, as the post dialog is by the token and a clean exit; a
/// clean exit without the token is unavailable, as the post dialog's is.
async fn confirm_review(count: VoucherCount, preview: &str) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|_| "ack_review_unavailable")?;
    confirm_review_with(&executable, count, preview).await
}

async fn confirm_review_with(
    executable: &std::path::Path,
    count: VoucherCount,
    preview: &str,
) -> Result<(), String> {
    if preview.len() > MAX_PREVIEW_BYTES {
        return Err("ack_review_too_large".into());
    }
    match nonce_bound_dialog(
        executable,
        "--confirm-review",
        REVIEW_TOKEN_PREFIX,
        count,
        preview,
    )
    .await
    {
        Ok(answer) if answer.token_matched => Ok(()),
        // A person's decline is no token and exit 1: `run_review_confirmation`
        // returns false. A clean exit without the token is never that; it is
        // an executable that is not this dialog, such as one ignoring the
        // flag (#689).
        Ok(answer) if answer.exited_cleanly => Err("ack_review_unavailable".into()),
        Ok(_) => Err("ack_review_declined".into()),
        Err(DialogFailure::Unavailable) => Err("ack_review_unavailable".into()),
        Err(DialogFailure::TimedOut) => Err("ack_review_timed_out".into()),
    }
}

/// What a dialog subprocess answered: whether it printed exactly the token
/// for this call's nonce, and whether it exited cleanly.
struct DialogAnswer {
    token_matched: bool,
    exited_cleanly: bool,
}

enum DialogFailure {
    Unavailable,
    TimedOut,
}

/// Show `preview` in the native dialog `mode` selects, in a subprocess of
/// `executable`, and read its answer. The parent sends a fresh nonce line, a
/// line with the voucher count the dialog's title and button name (#746),
/// and then the preview; the child prints `prefix` and that nonce only when
/// the person chose the positive button.
async fn nonce_bound_dialog(
    executable: &std::path::Path,
    mode: &str,
    prefix: &str,
    count: VoucherCount,
    preview: &str,
) -> Result<DialogAnswer, DialogFailure> {
    let nonce = uuid::Uuid::new_v4().to_string();
    #[expect(
        clippy::disallowed_methods,
        reason = "the native approval dialog helper, a local executable reached over stdin and stdout, not the network"
    )]
    let mut child = tokio::process::Command::new(executable)
        .arg(mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| DialogFailure::Unavailable)?;
    let (answer, status) = tokio::time::timeout(Duration::from_secs(120), async {
        let mut input = child.stdin.take().ok_or(DialogFailure::Unavailable)?;
        input
            .write_all(format!("{nonce}\n{}\n{preview}", count.0).as_bytes())
            .await
            .map_err(|_| DialogFailure::Unavailable)?;
        drop(input);
        // The answer is one short line, so at most 128 bytes are read. A
        // child that writes more without exiting is waited on until the
        // timeout, then killed on drop: bounded on purpose.
        let mut output = child.stdout.take().ok_or(DialogFailure::Unavailable)?;
        let mut answer = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(
            &mut tokio::io::AsyncReadExt::take(&mut output, 128),
            &mut answer,
        )
        .await
        .map_err(|_| DialogFailure::Unavailable)?;
        let status = child.wait().await.map_err(|_| DialogFailure::Unavailable)?;
        Ok::<_, DialogFailure>((answer, status))
    })
    .await
    .map_err(|_| DialogFailure::TimedOut)??;
    Ok(DialogAnswer {
        token_matched: answer == dialog_token(prefix, &nonce).as_bytes(),
        exited_cleanly: status.success(),
    })
}

fn dialog_token(prefix: &str, nonce: &str) -> String {
    format!("{prefix}{nonce}\n")
}

/// Entry point for the same executable's private native-dialog subprocess.
/// Runs before Tokio starts, because macOS dialogs require the main thread.
/// It prints the token for the nonce it was given only when the person chose
/// to post (#635); the parent trusts nothing else.
pub fn run_confirmation() -> bool {
    answer_with_token(
        POST_TOKEN_PREFIX,
        show_review,
        std::io::stdin(),
        std::io::stdout(),
    )
}

/// Entry point for the review dialog's subprocess (#239), under the same rules.
pub fn run_review_confirmation() -> bool {
    answer_with_token(
        REVIEW_TOKEN_PREFIX,
        show_review_acknowledgement,
        std::io::stdin(),
        std::io::stdout(),
    )
}

/// Read the parent's nonce line, count line and preview from `input`, show
/// `dialog` with that count and preview, and
/// write the token for that nonce to `output` only when it returns true. The
/// entry points pass stdin, stdout and their own dialog. The parent's tests
/// stand a script in for the child, so taking these as parameters is the only
/// way a test reaches the one line that turns a click into an approval: a
/// declined dialog writes nothing (#687).
fn answer_with_token(
    prefix: &str,
    dialog: fn(VoucherCount, &str) -> bool,
    input: impl Read,
    mut output: impl std::io::Write,
) -> bool {
    let mut text = String::new();
    // The nonce line is 37 bytes and the count line at most 21, so 64 covers
    // both: a preview at the limit is still read whole.
    if input
        .take(MAX_PREVIEW_BYTES as u64 + 64)
        .read_to_string(&mut text)
        .is_err()
    {
        return false;
    }
    let Some((nonce, count, preview)) = dialog_input(&text) else {
        return false;
    };
    if !dialog(count, preview) {
        return false;
    }
    output
        .write_all(dialog_token(prefix, nonce).as_bytes())
        .is_ok()
        && output.flush().is_ok()
}

/// The nonce line, the voucher count and the preview, when the input has the
/// shape the parent sends; `None` shows no dialog.
fn dialog_input(input: &str) -> Option<(&str, VoucherCount, &str)> {
    let (nonce, rest) = input.split_once('\n')?;
    let (count, preview) = rest.split_once('\n')?;
    let count = VoucherCount::parse(count)?;
    (uuid::Uuid::parse_str(nonce).is_ok()
        && !preview.contains('\0')
        && !preview.is_empty()
        && preview.len() <= MAX_PREVIEW_BYTES)
        .then_some((nonce, count, preview))
}

/// The acknowledgement dialog's title: the single-voucher words for one, and
/// the count for a batch (#746).
#[cfg(not(windows))]
fn review_title(count: VoucherCount) -> String {
    match count.batch() {
        None => "ComplyEaze Bridge — record that you reviewed one voucher".into(),
        Some(count) => format!("ComplyEaze Bridge — record that you reviewed {count} vouchers"),
    }
}

/// The acknowledgement dialog. It posts nothing, so neither its title nor its
/// button may read as approving a post.
#[cfg(not(windows))]
fn show_review_acknowledgement(count: VoucherCount, preview: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title(review_title(count))
        .set_description(preview)
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(rfd::MessageButtons::OkCancelCustom(
            "Cancel".into(),
            REVIEW_BUTTON.into(),
        ))
        .show()
        == rfd::MessageDialogResult::Custom(REVIEW_BUTTON.into())
}

/// The Windows acknowledgement dialog's title. Its Yes/No/Cancel box has no
/// custom labels, so the title asks the question, with the count for a batch
/// (#746).
#[cfg(windows)]
fn review_question(count: VoucherCount) -> String {
    match count.batch() {
        None => "ComplyEaze Bridge — record that you reviewed this voucher?".into(),
        Some(count) => {
            format!("ComplyEaze Bridge — record that you reviewed these {count} vouchers?")
        }
    }
}

#[cfg(windows)]
fn show_review_acknowledgement(count: VoucherCount, preview: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_YESNOCANCEL,
    };
    let text: Vec<u16> = preview.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = review_question(count)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: as for `show_review`: both buffers are NUL-terminated and live
    // for the synchronous dialog, and no parent HWND is borrowed.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNOCANCEL | MB_DEFBUTTON2 | MB_ICONWARNING | MB_SETFOREGROUND,
        ) == IDYES
    }
}

/// The post dialog's title and positive button. One voucher keeps the
/// single-voucher words; a batch names its count in both, so neither calls a
/// batch one voucher (#746).
#[cfg(not(windows))]
fn post_words(count: VoucherCount) -> (String, String) {
    match count.batch() {
        None => (
            "ComplyEaze Bridge — approve one voucher".into(),
            POST_LABEL.into(),
        ),
        Some(count) => (
            format!("ComplyEaze Bridge — approve {count} vouchers"),
            format!("Post {count} vouchers"),
        ),
    }
}

#[cfg(not(windows))]
fn show_review(count: VoucherCount, preview: &str) -> bool {
    let (title, button) = post_words(count);
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(preview)
        .set_level(rfd::MessageLevel::Warning)
        // The Cancel label supplies the native Escape action. Posting requires
        // the explicitly matched positive button; Return may leave this dialog open.
        .set_buttons(rfd::MessageButtons::OkCancelCustom(
            "Cancel".into(),
            button.clone(),
        ))
        .show()
        == rfd::MessageDialogResult::Custom(button)
}

/// The Windows post dialog's title. Its Yes/No/Cancel box has no custom
/// labels, so the title asks the question, with the count for a batch (#746).
#[cfg(windows)]
fn post_question(count: VoucherCount) -> String {
    match count.batch() {
        None => "ComplyEaze Bridge — post this voucher?".into(),
        Some(count) => format!("ComplyEaze Bridge — post {count} vouchers?"),
    }
}

#[cfg(windows)]
fn show_review(count: VoucherCount, preview: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_YESNOCANCEL,
    };
    // rfd without common-controls-v6 discards custom labels. Use the existing
    // Win32 dependency so No is the default and Escape/close remain Cancel.
    let text: Vec<u16> = preview.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = post_question(count).encode_utf16().chain(Some(0)).collect();
    // SAFETY: Both buffers are NUL-terminated and live for the synchronous dialog;
    // no parent HWND is borrowed. No application state is exposed to callbacks.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNOCANCEL | MB_DEFBUTTON2 | MB_ICONWARNING | MB_SETFOREGROUND,
        ) == IDYES
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("import_company_scope_ambiguous")]
pub(crate) struct AmbiguousImportCompany;

pub(super) fn require_unique_company_scope(
    companies: &[bridge_tally_protocol::TallyCompany],
    identity: &super::VerifiedCompanyIdentity,
) -> Result<(), AmbiguousImportCompany> {
    let count = companies
        .iter()
        .filter(|company| {
            company
                .name
                .trim()
                .eq_ignore_ascii_case(identity.display_name().trim())
        })
        .count();
    if count == 1 {
        Ok(())
    } else {
        Err(AmbiguousImportCompany)
    }
}

#[cfg(test)]
#[path = "approved_import_tests.rs"]
mod tests;
