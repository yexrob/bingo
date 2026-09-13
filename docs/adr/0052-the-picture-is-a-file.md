# ADR-0052 — The picture is a file

Status: accepted · 2026-09-09 · Plan: M87 · Amends: ADR-0040 §1, §4

## Context

ADR-0040 let a picture cross from a person to the journal as bytes:
a paste is held beside the composer's line, an `@word` is read at
submit, a chat picture is fetched into memory. The model sees every
one of them. But a model that wants to *hand a picture on* — to a
script, a `--image` flag, an upload — needs a file, and a pasted or
chatted picture has none: the only copy on disk is the base64 in the
journal. Observed 2026-09-09: the model searched three directories
for a file that was never written, then cut it out of the journal.

The ratchet's question: would refusing a path on `Image` force a
second representation? Yes — each surface would spell "where this
picture is" in its own text, or the kernel would rewrite the person's
words, and the journal would hold the bytes and the words apart.

## Decision

1. **A picture a person hands in is a file first.** The surface that
   takes it writes it before anything else sees it: a paste under
   `<data>/pictures/pasted/<hash>.<ext>`, a chat attachment under the
   message's directory with the other attachments. An `@word` is a
   file already. There is one way from a person to the journal — a
   path, read into memory — and no surface holds bytes that are on no
   disk.
2. **`Image` knows where it is.** `Image { media_type, data, path }`,
   `path: Option<PathBuf>`, absent from the wire and the journal when
   there is none. `Image::read` and `bingo_pictures::load` of a path
   set it; bytes from a tool, a wire client or a fetched URL leave it
   unset. It is a location, not a second copy: the bytes are the
   picture, the path is where the same bytes are.
3. **The words are the person's.** `[image N]` and the line stay as
   typed. A provider tells the model the path beside the picture it
   encodes (`Image::whereabouts`, one spelling), and a model that
   cannot see is told the path in place of the picture. Nothing is
   added to the item; the journal keeps what was sent.
4. **The kernel still does no file I/O for input** (ADR-0040 §2). A
   client that sends bytes inline over the wire gets no path: it had
   no file, and the kernel will not write one for it.

## Consequences

- `pictures::Held` in the TUI holds paths, not pictures; the strip and
  the viewer read a draft through the same memo that reads an
  answer's `![…](path)`, and a click opens the pasted file itself.
- One atomic writer (`bingo_pictures::file::written`) replaces the
  cache's and the Feishu adapter's own.
- The RPC schema gains one optional field; no recorded frame changes.
- A deleted pasted file breaks a tool call that names it, not the
  turn: the journal still has the bytes and the transcript still draws.

Refs: ADR-0040, ADR-0041, ADR-0051; Plan: M87
