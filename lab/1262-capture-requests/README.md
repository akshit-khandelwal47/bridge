# #1262 lab sitting: a voucher that carries a ledger's stored name while the row attribute differs

Synthetic companies only. Not for merge.

## What this is for

This sitting is meant to give one capture where, for one ledger of one synthetic company, both of these hold:

- in the `List of Ledgers` read that `vouchers` sends (`q1`), the row's `NAME="…"` attribute differs from the first `NAME` under `LANGUAGENAME.LIST/NAME.LIST` (the stored name);
- in the voucher read for the same day (`q2`), that voucher's `LEDGERNAME` is the stored name.

It is the failing state of #1262. `q3` and `q4` are the two reads `ledger_movement` sends, for the second PR.

## The recipe is a guess

Nobody knows how to produce this state:

- In #1342, a rename that changed only letter case left the attribute in the old case. The vouchers carried the attribute. Later both moved to the new case together, and what triggered that was not established.
- The issue's own untested explanation is that Tally chooses a spelling shared by the companies loaded at the time.

This sitting tests that explanation. Two synthetic companies hold the same ledger name, differing only in case, and are loaded in different orders. It may not produce the state. If it does not, we stop and report. Nothing else is tried in this sitting.

## Rules

- TallyPrime on the lab laptop, port 9001. `BRIDGE PILOT LAB` and every other company stay closed throughout.
- Akshit takes a fresh backup of the data folder before step 1.
- Akshit keys everything in Tally's own screens, and ComplyEaze Bridge sends nothing. The only writes are steps 1 and 2. Steps K3 to K5 only exit Tally, start it and load companies.
- The requests (`q1` to `q4`) are read-only. Check each file against `SHA256.txt` before sending, and send them as described under "Sending". Write every answer to a file. An empty answer, an answer with an error in it, or any HTTP status other than 200 is a failed read: keep it and say so.
- This runs last on the laptop, after the #1342 W1 to W4 answers, the write sitting and #1500 Part 1. It takes about an hour. If the laptop's time runs out first, it waits for the next sitting.
- Every request names the company `BRIDGE SPELL LAB` and the day 1-Apr-2026.

## Sending

Send one file at a time, as the bytes in the file (UTF-16 with its byte-order mark). For example:

```
curl.exe -sS -w "%{http_code}" --data-binary "@q1-ledger-catalogue-v1.xml" -H "Content-Type: text/xml; charset=utf-16" http://127.0.0.1:9001 -o K1-q1-ledger-catalogue-v1.xml
```

Record the status code curl prints with each answer.

## Which companies are loaded: shown, not assumed

TallyPrime can open companies by itself when it starts.

- Use only the regular TallyPrime on port 9001, with TallyPrime Edit Log closed.
- After each start (K3 to K5), and before loading anything, take a screenshot of the Gateway of Tally showing which companies are open.
- If a company is open that the checkpoint does not name, close it. Then load in the stated order.
- After each start, check that `http://127.0.0.1:9001` answers before sending `q1`.
- Immediately before each checkpoint's first request (K1 and K2 included), take one more screenshot of the open companies.

## Stop rules

- If a request has not answered in 10 minutes, or a dialog opens in Tally's own window, send nothing more. Take a screenshot of the window and the dialog, and tell us before restarting.
- A start after which port 9001 does not answer is a stop too, not a retry.

## The two companies (writes)

**Step 1. `BRIDGE SPELL LAB`** (company A)

- Books from 1-Apr-2026, India, no GST, no inventory.
- Ledger `Bridge Spell Party` (exactly this case), under Sundry Debtors. No opening balance; "Maintain balances bill-by-bill" No.
- One Receipt voucher dated 1-Apr-2026: Cash debit 100.00, `Bridge Spell Party` credit 100.00. No narration. Automatic number.
- Then run checkpoint **K1**, with A the only company loaded.

**Step 2. `BRIDGE SPELL TWIN`** (company B), created while A stays loaded

- Books from 1-Apr-2026.
- Ledger `BRIDGE SPELL PARTY` (all capitals), under Sundry Debtors. No opening balance, no vouchers.
- Then run checkpoint **K2**, with A and B both loaded.

## Checkpoints (reads only)

At each checkpoint, send `q1`, `q2`, `q3` and `q4`, in that order. Save each answer as `<checkpoint>-<request file name>`, for example `K3-q2-vouchers-day.xml`.

| Checkpoint | Tally state before the reads |
| --- | --- |
| K1 | after step 1: A alone |
| K2 | after step 2: A and B loaded |
| K3 | exit Tally fully, start it, load **B first, then A** |
| K4 | exit Tally fully, start it, load **A first, then B** |
| K5 | exit Tally fully, start it, load **A alone** |

Run all five, even if an earlier one already shows the state: each is four small reads. Record for each checkpoint which companies were loaded and in what order, and whether Tally showed any message.

## How the checkpoints are compared

One thing changes at a time between the checkpoints of each pair:

- **K1 against K2:** the second company created and loaded.
- **K3 against K4:** the load order only.
- **K5 against K1:** a restart only.

Report each checkpoint's answers in that frame, with the screenshots from "Which companies are loaded".

## What to send back

- The 20 answers, plus the HTTP status, the size in bytes and the SHA-256 of each.
- The screenshots: one after each start (K3 to K5), and one before each checkpoint's first request.
- The backup file name. The backup itself stays on the laptop.
- A note of anything unexpected.

Do not commit or push from the laptop. Copy the answers to the Mac, and they will be committed to `lab/1262-capture-answers` on the fork with a `SHA256.txt`.

## How the answers will be read (by structure, not grep)

For the `Bridge Spell Party` ledger, compare:

- `q1` and `q4`: the row's `NAME` attribute against the first `NAME` under `LANGUAGENAME.LIST/NAME.LIST`;
- `q2` and `q3`: the `LEDGERNAME` of the voucher's entry for that ledger, and `PARTYLEDGERNAME` where it is present.

A checkpoint shows the failing state only if, in `q1`, the attribute differs from the stored name, and the voucher's `LEDGERNAME` in `q2` equals the stored name.

## The requests

The four requests were rendered from master `18580f05`, through the functions the tools call. Each was encoded as UTF-16LE with a BOM by `encode_tally_xml_request_utf16le`, which is the encoding the transport sends.

| File | Rendered by | Sent by |
| --- | --- | --- |
| `q1-ledger-catalogue-v1.xml` | `standard_ledger_catalog_read` | `vouchers` with `ledger` (both catalogue reads) |
| `q2-vouchers-day.xml` | `render_agent_vouchers`, one day, no span | `vouchers`, one data part |
| `q3-movement-vouchers-day.xml` | `render_agent_movement_vouchers`, one day | `ledger_movement`, one data part |
| `q4-movement-ledgers.xml` | `render_native_ledger_export_request`, period 20260401 to 20260401 | `ledger_movement`'s ledger list, opening at 1-Apr-2026 |

The tools also send identity, marks and census reads around these four. They are not needed for this question, so they are left out.
