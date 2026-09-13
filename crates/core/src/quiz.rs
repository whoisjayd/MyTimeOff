//! What a quiz is, and the rules for getting past one.
//!
//! Nothing here generates questions - that needs a book, a model and a network, none of
//! which belong in this crate. What lives here is the part worth being certain about:
//! given a quiz, a mode and an answer sheet, may you leave? Those rules are the entire
//! point of the tool, and they are pure functions of their inputs.

use serde::{Deserialize, Serialize};

use crate::machine::ReaderMode;
use crate::reading::Locator;

/// One multiple-choice question, drawn from one page.
///
/// Multiple choice only, deliberately. A free-text answer would need a model to mark it,
/// which puts a network round trip between you and your own screen - and a marker that
/// can be argued with is a marker that will be argued with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub prompt: String,
    pub choices: Vec<String>,
    /// Index into `choices`. Never sent to a surface; see [`Quiz::for_display`].
    pub answer_index: usize,
    /// The page this was drawn from, so a wrong answer can point at what to re-read.
    pub source: Locator,
}

/// A question as a reader surface is allowed to see it.
///
/// The answer is stripped rather than trusted to stay unread: the reader runs in a
/// browser with a devtools console, so a quiz that ships its own answer key is not a
/// quiz. Marking happens in the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskedQuestion {
    pub id: String,
    pub prompt: String,
    pub choices: Vec<String>,
    pub source: Locator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quiz {
    pub id: String,
    pub book_id: String,
    pub questions: Vec<Question>,
    /// Wall-clock milliseconds, so a cached quiz can be aged out.
    pub generated_at: i64,
    /// True when this came from the pre-generation cache rather than being made on demand.
    pub pre_generated: bool,
}

impl Quiz {
    /// The pages this quiz was drawn from, in order and without repeats.
    pub fn span(&self) -> Vec<Locator> {
        let mut span: Vec<Locator> = Vec::new();
        for question in &self.questions {
            if !span.contains(&question.source) {
                span.push(question.source.clone());
            }
        }
        span
    }

    /// The quiz with every answer removed.
    pub fn for_display(&self) -> Vec<AskedQuestion> {
        self.questions
            .iter()
            .map(|question| AskedQuestion {
                id: question.id.clone(),
                prompt: question.prompt.clone(),
                choices: question.choices.clone(),
                source: question.source.clone(),
            })
            .collect()
    }

    /// Marks a submission.
    ///
    /// Iterates the questions rather than the answers, so an answer naming a question
    /// that is not on this quiz is ignored and the same question answered twice is
    /// counted once. Neither can be turned into a better score.
    pub fn mark(&self, submission: &Submission) -> Score {
        let mut correct = 0;
        let mut answered = 0;
        for question in &self.questions {
            let Some(choice) = submission.choice_for(&question.id) else { continue };
            answered += 1;
            if choice == question.answer_index {
                correct += 1;
            }
        }
        Score { correct, answered, total: self.questions.len() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub question_id: String,
    /// Index into that question's `choices`.
    pub choice: usize,
}

/// One filled-in answer sheet.
///
/// The quiz id travels with it so a sheet from a quiz that has since been replaced can be
/// refused rather than marked against the wrong questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub quiz_id: String,
    pub answers: Vec<Answer>,
}

impl Submission {
    fn choice_for(&self, question_id: &str) -> Option<usize> {
        self.answers.iter().find(|a| a.question_id == question_id).map(|a| a.choice)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    pub correct: usize,
    /// Questions with an answer, right or wrong.
    pub answered: usize,
    pub total: usize,
}

impl Score {
    /// The score as a fraction, for showing to a person.
    ///
    /// Out of `total`, not `answered`: otherwise answering one question correctly and
    /// leaving the rest blank would read as a perfect 1.0. Nothing decides anything on
    /// this number - see [`PassMark`].
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        self.correct as f32 / self.total as f32
    }
}

/// The bar, as a ratio rather than a percentage.
///
/// "Two of every three" instead of 0.67, because the obvious version of this is a float
/// comparison and the obvious version is wrong: two correct out of three is 0.6666667,
/// which is not >= 0.67, and a gate that fails a passing answer sheet is the single worst
/// bug this tool could have. Cross-multiplying integers has no such rounding to get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassMark {
    pub correct: u32,
    pub of: u32,
}

impl PassMark {
    /// Whether `score` clears the bar: correct/total >= correct/of, without dividing.
    pub fn met(&self, score: &Score) -> bool {
        // An empty quiz cannot be failed. A gate with nothing to ask - too little read to
        // draw a question from - must not trap anyone behind it.
        if score.total == 0 {
            return true;
        }
        (score.correct as u64) * (self.of as u64) >= (self.correct as u64) * (score.total as u64)
    }

    /// The fewest right answers out of `total` that clear this bar.
    ///
    /// The same arithmetic as [`met`](Self::met), asked forwards, and here so that it is
    /// asked in one language rather than two. A surface has to tell somebody what the
    /// gate costs *before* they answer, and the ratio alone does not say: "two of every
    /// three" on a sheet with one question on it reads as a bar that cannot be cleared,
    /// when in fact one right answer clears it.
    pub fn needed(&self, total: u32) -> u32 {
        // ceil(correct * total / of), in integers. Clamped because a bar set above its
        // own denominator still cannot ask for more answers than were asked for.
        let wanted = u64::from(self.correct) * u64::from(total);
        wanted.div_ceil(u64::from(self.of).max(1)).min(u64::from(total)) as u32
    }
}

/// What a mode actually means, once you stop describing it in adjectives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Whether leaving involves a quiz at all.
    pub quiz: bool,
    /// Whether an empty answer sheet is refused rather than marked as zero.
    pub require_attempt: bool,
    /// How much of the quiz must be correct. None means any attempt is enough.
    pub pass_mark: Option<PassMark>,
    /// How many further attempts a failure buys. The locked rule is retry once, then
    /// release - so this is one, and after that the gate opens however you did.
    pub retries: u32,
    /// Whether the gate can be dismissed without being marked.
    pub skippable: bool,
}

impl Policy {
    pub fn for_mode(mode: ReaderMode) -> Policy {
        match mode {
            // The one mode that can actually keep you there, and only for one retry.
            ReaderMode::Strict => Policy {
                quiz: true,
                require_attempt: true,
                pass_mark: Some(PassMark { correct: 2, of: 3 }),
                retries: 1,
                skippable: false,
            },
            // A quiz you are asked but not made to sit. `require_attempt` is false rather
            // than true-with-an-escape-hatch: where skipping is allowed, refusing a blank
            // sheet would only mean pressing a different button for the same result.
            ReaderMode::Lenient => Policy {
                quiz: true,
                require_attempt: false,
                pass_mark: None,
                retries: 0,
                skippable: true,
            },
            ReaderMode::Free => Policy {
                quiz: false,
                require_attempt: false,
                pass_mark: None,
                retries: 0,
                skippable: true,
            },
        }
    }

    /// The answer to "may I just leave?".
    ///
    /// It lives on the policy rather than the gate because it depends on nothing else: no
    /// questions, no score, no attempts. That matters at the other end, where a reader can
    /// press skip before the questions have been generated - and in strict mode, where the
    /// refusal must not depend on a generator having answered first.
    pub fn skip(&self) -> Verdict {
        if self.skippable {
            Verdict::Released { reason: Release::Skipped, score: None }
        } else {
            Verdict::Refused { reason: Refusal::NotSkippable }
        }
    }
}

/// Why the gate opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Release {
    /// Scored at or above the pass mark.
    Passed,
    /// Marked, with no pass mark to meet.
    Attempted,
    /// Failed, and the retry allowance is spent. The tool nags; it does not imprison.
    Exhausted,
    /// Dismissed without being marked, where the mode allows that.
    Skipped,
    /// The gate had nothing to ask - too little read, or no way to reach a generator.
    /// A tool that cannot pose a question has not earned the right to hold the screen.
    Ungated,
}

/// Why a submission was not marked at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// A blank sheet, where the mode requires an attempt.
    NotAttempted,
    /// An answer sheet for a different quiz than the one that is open.
    WrongQuiz,
    /// Asked to skip in a mode that does not allow it.
    NotSkippable,
}

/// The outcome of one interaction with the gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Verdict {
    Released { reason: Release, score: Option<Score> },
    Retry { score: Score, attempts_left: u32 },
    Refused { reason: Refusal },
}

/// One open gate: a quiz, the mode it is being taken under, and what has been tried.
///
/// Holds the attempt count because the retry rule is the only thing here with a memory.
/// The state machine deliberately does not know about any of this - it only ever learns
/// that the gate is done.
#[derive(Debug, Clone)]
pub struct Gate {
    policy: Policy,
    quiz: Quiz,
    attempts: u32,
}

impl Gate {
    pub fn open(mode: ReaderMode, quiz: Quiz) -> Gate {
        Gate { policy: Policy::for_mode(mode), quiz, attempts: 0 }
    }

    pub fn policy(&self) -> Policy {
        self.policy
    }

    pub fn quiz(&self) -> &Quiz {
        &self.quiz
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Attempts left after the one just taken, or before the first if none has been.
    pub fn attempts_left(&self) -> u32 {
        (self.policy.retries + 1).saturating_sub(self.attempts)
    }

    /// Marks a submission and decides whether that ends the gate.
    ///
    /// Takes `&mut self` because a failed attempt is spent whether or not it opened the
    /// gate; that is what makes the retry allowance finite.
    pub fn submit(&mut self, submission: &Submission) -> Verdict {
        if submission.quiz_id != self.quiz.id {
            return Verdict::Refused { reason: Refusal::WrongQuiz };
        }

        let score = self.quiz.mark(submission);
        // `total > 0` because a quiz with no questions has nothing to attempt, and
        // demanding an attempt at it would be a gate that can never open.
        if self.policy.require_attempt && score.total > 0 && score.answered == 0 {
            // Not counted as an attempt: refusing to mark a blank sheet and then charging
            // for it would spend the retry allowance on nothing.
            return Verdict::Refused { reason: Refusal::NotAttempted };
        }

        self.attempts += 1;
        match self.policy.pass_mark {
            None => Verdict::Released { reason: Release::Attempted, score: Some(score) },
            Some(mark) if mark.met(&score) => {
                Verdict::Released { reason: Release::Passed, score: Some(score) }
            }
            Some(_) if self.attempts_left() == 0 => {
                Verdict::Released { reason: Release::Exhausted, score: Some(score) }
            }
            Some(_) => Verdict::Retry { score, attempts_left: self.attempts_left() },
        }
    }

    /// Dismisses the gate without marking it.
    pub fn skip(&self) -> Verdict {
        self.policy.skip()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(n: usize, answer_index: usize) -> Question {
        Question {
            id: format!("q{n}"),
            prompt: format!("question {n}"),
            choices: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            answer_index,
            source: Locator::Page { page: n as u32, page_label: n.to_string() },
        }
    }

    /// Three questions, all answered by choice 0.
    fn quiz() -> Quiz {
        Quiz {
            id: "quiz-1".into(),
            book_id: "book-1".into(),
            questions: vec![question(1, 0), question(2, 0), question(3, 0)],
            generated_at: 0,
            pre_generated: false,
        }
    }

    /// The bar a surface shows and the bar the daemon marks against must be the same bar.
    ///
    /// Checked against `met` rather than against the formula, because the formula is the
    /// thing under test: if `needed` ever disagrees, the gate tells people one price and
    /// charges another.
    #[test]
    fn what_a_gate_costs_is_the_same_number_it_marks_against() {
        let mark = PassMark { correct: 2, of: 3 };
        for total in 0..=12u32 {
            let needed = mark.needed(total);
            assert!(needed <= total, "{needed} of {total} is more than was asked");
            for correct in 0..=total {
                let score = Score { correct: correct as usize, total: total as usize, answered: 0 };
                assert_eq!(
                    mark.met(&score),
                    correct >= needed,
                    "{correct} of {total} against a bar of {needed}",
                );
            }
        }
    }

    /// The case that started this: one page read, so one question asked.
    #[test]
    fn a_single_question_under_a_two_in_three_bar_needs_one_right_answer() {
        assert_eq!(PassMark { correct: 2, of: 3 }.needed(1), 1);
    }

    /// An answer sheet where the first `right` questions are correct and the rest wrong.
    fn sheet(right: usize) -> Submission {
        Submission {
            quiz_id: "quiz-1".into(),
            answers: (1..=3)
                .map(|n| Answer {
                    question_id: format!("q{n}"),
                    choice: if n <= right { 0 } else { 1 },
                })
                .collect(),
        }
    }

    fn blank() -> Submission {
        Submission { quiz_id: "quiz-1".into(), answers: Vec::new() }
    }

    #[test]
    fn a_passing_score_opens_the_gate() {
        let mut gate = Gate::open(ReaderMode::Strict, quiz());
        let verdict = gate.submit(&sheet(2));
        assert!(
            matches!(verdict, Verdict::Released { reason: Release::Passed, .. }),
            "{verdict:?}"
        );
    }

    /// Regression: 2/3 is 0.6666667, and a float pass mark of 0.67 failed it.
    #[test]
    fn two_correct_out_of_three_is_a_pass() {
        let mark = PassMark { correct: 2, of: 3 };
        assert!(mark.met(&Score { correct: 2, answered: 3, total: 3 }));
        assert!(mark.met(&Score { correct: 4, answered: 6, total: 6 }));
        assert!(!mark.met(&Score { correct: 1, answered: 3, total: 3 }));
    }

    #[test]
    fn a_failing_score_buys_exactly_one_retry() {
        let mut gate = Gate::open(ReaderMode::Strict, quiz());
        let first = gate.submit(&sheet(0));
        assert!(matches!(first, Verdict::Retry { attempts_left: 1, .. }), "{first:?}");

        let second = gate.submit(&sheet(0));
        assert!(
            matches!(second, Verdict::Released { reason: Release::Exhausted, .. }),
            "the tool nags, it does not imprison: {second:?}"
        );
    }

    #[test]
    fn the_retry_can_still_be_passed() {
        let mut gate = Gate::open(ReaderMode::Strict, quiz());
        gate.submit(&sheet(0));
        let verdict = gate.submit(&sheet(3));
        assert!(
            matches!(verdict, Verdict::Released { reason: Release::Passed, .. }),
            "{verdict:?}"
        );
    }

    #[test]
    fn a_blank_sheet_is_refused_without_spending_the_retry() {
        let mut gate = Gate::open(ReaderMode::Strict, quiz());
        let refused = gate.submit(&blank());
        assert_eq!(refused, Verdict::Refused { reason: Refusal::NotAttempted });
        assert_eq!(gate.attempts(), 0, "a sheet that was never marked is not an attempt");

        // Still gets its full allowance afterwards.
        assert!(matches!(gate.submit(&sheet(0)), Verdict::Retry { attempts_left: 1, .. }));
    }

    #[test]
    fn strict_mode_cannot_be_skipped() {
        let gate = Gate::open(ReaderMode::Strict, quiz());
        assert_eq!(gate.skip(), Verdict::Refused { reason: Refusal::NotSkippable });
    }

    #[test]
    fn lenient_mode_releases_on_any_attempt_however_bad() {
        let mut gate = Gate::open(ReaderMode::Lenient, quiz());
        let verdict = gate.submit(&sheet(0));
        assert!(
            matches!(verdict, Verdict::Released { reason: Release::Attempted, .. }),
            "{verdict:?}"
        );
    }

    #[test]
    fn lenient_mode_can_be_skipped() {
        let gate = Gate::open(ReaderMode::Lenient, quiz());
        assert_eq!(gate.skip(), Verdict::Released { reason: Release::Skipped, score: None });
    }

    #[test]
    fn a_sheet_for_another_quiz_is_refused() {
        // A reader that reconnected and answered a quiz the daemon has since replaced.
        let mut gate = Gate::open(ReaderMode::Strict, quiz());
        let stale = Submission { quiz_id: "quiz-0".into(), ..sheet(3) };
        assert_eq!(gate.submit(&stale), Verdict::Refused { reason: Refusal::WrongQuiz });
        assert_eq!(gate.attempts(), 0);
    }

    #[test]
    fn answering_one_question_twice_counts_once() {
        let submission = Submission {
            quiz_id: "quiz-1".into(),
            answers: vec![
                Answer { question_id: "q1".into(), choice: 1 },
                Answer { question_id: "q1".into(), choice: 0 },
            ],
        };
        let score = quiz().mark(&submission);
        assert_eq!(score.answered, 1, "a second answer to the same question is not a second try");
        assert_eq!(score.correct, 0, "and the first one stands");
    }

    #[test]
    fn an_answer_to_a_question_that_is_not_asked_is_ignored() {
        let submission = Submission {
            quiz_id: "quiz-1".into(),
            answers: vec![Answer { question_id: "q9".into(), choice: 0 }],
        };
        assert_eq!(quiz().mark(&submission), Score { correct: 0, answered: 0, total: 3 });
    }

    #[test]
    fn unanswered_questions_count_against_the_score() {
        let submission = Submission {
            quiz_id: "quiz-1".into(),
            answers: vec![Answer { question_id: "q1".into(), choice: 0 }],
        };
        // One right out of three, not one out of one.
        assert!((quiz().mark(&submission).fraction() - 1.0 / 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn an_empty_quiz_cannot_trap_anyone() {
        let empty = Quiz { questions: Vec::new(), ..quiz() };
        let mut gate = Gate::open(ReaderMode::Strict, empty);
        let verdict = gate.submit(&Submission { quiz_id: "quiz-1".into(), answers: Vec::new() });
        assert!(
            matches!(verdict, Verdict::Released { reason: Release::Passed, .. }),
            "nothing was asked, so nothing can be got wrong: {verdict:?}"
        );
    }

    #[test]
    fn the_answer_key_never_leaves_the_daemon() {
        let asked = quiz().for_display();
        let serialised = serde_json::to_string(&asked).expect("serialise");
        assert!(!serialised.contains("answer"), "{serialised}");
        assert_eq!(asked.len(), 3, "and every question still gets asked");
    }

    #[test]
    fn a_quiz_knows_which_pages_it_came_from() {
        let repeated =
            Quiz { questions: vec![question(1, 0), question(1, 1), question(2, 0)], ..quiz() };
        assert_eq!(repeated.span().len(), 2, "two questions from one page is still one page");
    }
}
