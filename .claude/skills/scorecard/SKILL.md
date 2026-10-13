---
name: scorecard
description: "Run and read the extraction scorecard and the text bench: the scorecard test, accepting new baseline scores, seeding ledgers for new documents, the known baseline gaps, and text-bench.py with --ocr-models. Use when measuring whether an extraction change made the output better or worse."
---

# The scorecard and the text bench

The rules live in `CLAUDE.md` ("Every extraction is scored" and "The ledgers
are ground truth"): the score may only go up, and a ledger is never edited to
make a test pass. This is how to run them.

```sh
cargo test -p pdf-toolkit --test scorecard -- --nocapture   # the extraction scorecard
PDF_TOOLKIT_SCORECARD_UPDATE=1 cargo test -p pdf-toolkit --test scorecard  # accept new scores
python3 scripts/seed-expected.py            # ledgers for documents that have none
```

Accept new scores only when every kind held or rose; the update makes the new
numbers the committed baseline (`tests/scorecard/baseline.json`).

Known gaps the baseline carries on purpose — the ledger is right and the
toolkit is wrong: a mis-read digit in dossier 10's entity, and an unread
hectare figure on dossier 14 p16. Re-check both against the pages before
quoting them; they are dated.

## Text bench

`scripts/text-bench.py` grades text extraction against other extractors.
Always pass `--ocr-models <dir>`: without it the scan page is read with no OCR
and the score drops to 1159/1204, against a baseline of 1204/1204.
