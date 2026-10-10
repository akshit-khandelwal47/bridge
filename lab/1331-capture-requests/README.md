# #1331 lab request: the parent filter on the ledger rates request

Read-only. Synthetic company `BRIDGE PILOT LAB` only. Not for merge.

## What this is for

#1331 scopes the invoice's ledger reads to the parent groups of the ledgers the invoice names. The `$Parent = "a" OR $Parent = "b"` filter is measured on the compliance listing and the balance collection (reference §11e). It has never been sent on the **rates** request, which also fetches `GSTDETAILS.LIST`, `RATEOFTAXCALCULATION`, `ROUNDINGMETHOD` and `ROUNDINGLIMIT`. Two questions, one request each:

- **q1:** with the four parents of the lab invoices, does the rates request answer exactly the ledgers under them, with all its fields?
- **q2:** with one parent spelled with a case change (`SALES ACCOUNTS`), does `$Parent` fold case? Folding returns the Sales Accounts ledgers; not folding returns a well-formed empty collection.

## The files

All four are exports (no import, no object action), name `BRIDGE PILOT LAB`, and are UTF-16LE with a byte-order mark. Check each against `SHA256.txt` before sending.

| File | What it asks | Where it comes from |
| --- | --- | --- |
| `q1-rates-four-parents.xml` | the rates request, filtered to `Duties & Taxes`, `Indirect Expenses`, `Sales Accounts`, `Sundry Debtors` | rendered by `render_ledger_rates_request_for_parents` at fork branch `gst-sales-scoped-ledger-reads-on-1513`, commit `c68e7ecec9468fc1f5626892f0270181e75c463e` (stacked on #1513's head `5518b0a8`), encoded by `encode_tally_xml_request_utf16le` |
| `q2-rates-one-parent-case-changed.xml` | the same, filtered to `SALES ACCOUNTS` | the same code |
| `q0-rates-whole.xml` | the unfiltered rates request, the one measured on 10 Oct | byte for byte the committed `pilot-lab-ledger-rates.request` fixture of #1513 (compared with `cmp`) |
| `q3-ledger-catalogue-v2.xml` | each ledger's name, group and bill-wise flag | byte for byte `r13-ledger-catalogue-v2.xml` of #1342's list (hash `28a47c26…1bd0`) |

`q1` and `q2` are the two measurements asked for. `q0` and `q3` are controls made of requests that have already been measured: they say, on the book as it is today, what the filtered answers should have contained. Without them the row counts of `q1` and `q2` have nothing to be compared with, because the book has changed since the 10 Oct capture.

Apart from the `SYSTEM` formula and the `FILTERS` element, `q1` and `q2` are `q0` unchanged (the test in the branch removes both and compares the rest with `q0`).

## Sending

Order: `q1`, `q2`, then `q3`, then `q0`. One file at a time, as the bytes in the file:

```
curl.exe -sS -w "%{http_code} %{size_download} %{time_total}" --data-binary "@q1-rates-four-parents.xml" -H "Content-Type: text/xml; charset=utf-16" http://127.0.0.1:9001 -o a-q1-rates-four-parents.xml
```

Save each answer as `a-` plus the request file name. Record the three numbers curl prints with each.

## Rules

- Regular TallyPrime on port 9001, TallyPrime Edit Log closed, `BRIDGE PILOT LAB` the only company loaded, Tally at the Gateway. Nothing is keyed or changed.
- Check `http://127.0.0.1:9001` answers before the first request.
- If a request has not answered in 10 minutes, or a dialog opens in Tally, send nothing more. Screenshot the window and the dialog, and tell us.
- An empty file, an HTTP status other than 200, or an answer with an error in it is a failed read: keep it and say so. A well-formed collection with no `LEDGER` rows is an answer, not a failure; keep it and say so as well.
- Do not edit or re-save any answer.

## What to report, per answer

Read by structure, not by search:

- HTTP status, bytes, seconds;
- the number of `LEDGER` elements, and how many sit under each `PARENT`;
- for `q1`, `q2` and `q0`: each row's GUID, so the filtered rows can be compared with `q0`'s rows;
- whether `GSTDETAILS.LIST`, `RATEOFTAXCALCULATION`, `ROUNDINGMETHOD` and `ROUNDINGLIMIT` are present in a filtered row exactly as in the same ledger's row of `q0`.

The answers are committed byte for byte, with their hashes, to `lab/1331-capture-answers` on the fork. Nothing is committed from the laptop.
