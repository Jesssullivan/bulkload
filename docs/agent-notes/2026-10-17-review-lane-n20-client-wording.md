# 2026-10-17: proposed client-deck wording for bulkload (Bulkload Reset Review row N20)

> **Status: PENDING.** This is a draft for the client lane to propose and
> the operator to review. It states the product goal, not a result. Do not
> present it as achieved until M0 evidence (the operator resumes real lanes
> on sting) and M3 evidence (end-to-end seat time against rclone on real
> estates, R-C842) exist.

Ruling: N20, operator decision 2026-10-17 (Linear TIN-4543 `63feb3cf`;
R-N13): "increase the client wording to fully encapsulate the product goal
(not merely soften) ... Client lane drafts; operator reviews." The deck lives
in a private client repository (`[repo]`); the full handoff, with the exact
lines to change, is on its Linear issue (TIN-5890). This repo holds only the
wording.

## Full-goal statement

> bulkload moves a developer's complete working estate to a new machine in
> place (git repos with linked worktrees, stashes and uncommitted work;
> Claude and Codex sessions; dotfiles), losing nothing, so work resumes
> exactly where it left off; measured end-to-end against rclone on real
> estates.

## What it replaces in the deck

- "a seat is disposable because its context moves with you" becomes "a
  seat becomes disposable once its whole working estate can move in place
  and resume exactly where it left off (goal under test)".
- "bulkload is the one-shot move" is dropped: reruns are routine (SEAT-11).
  It becomes "bulkload moves the complete working estate in place and reruns
  are cheap; tcfs is the roaming filesystem that takes over once it
  launches".
- "benchmarked against rclone on a fixed corpus" becomes "measured
  end-to-end against rclone on real estates (time to a working seat)"; the
  corpus race is a regression guard only (R-C842).
- Keep "has moved real estate between hosts" (true) and add "resume on the
  destination has not yet passed; that is the acceptance test" (README,
  corrected in #254).
