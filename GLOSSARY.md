# Glance

A desktop log viewer for opening and reading local log files, whatever their format.

## Language

**Log File**:
A local file whose content is log data, regardless of its extension (e.g. `.log` or `.txt`).
_Avoid_: Log, source file

**Format**:
The structure shared by every line of a Log File (e.g. plain text, JSON lines). A Log File has exactly one Format.
_Avoid_: Type, schema

**Entry**:
One logical record in a Log File. It starts at a line carrying a timestamp and includes any following lines without one (e.g. a stack trace). Filtering and bookmarking operate on Entries.
_Avoid_: Line, record, message

**Level**:
The severity of an Entry, normalized to one of TRACE, DEBUG, INFO, WARN, ERROR, FATAL regardless of how the Format writes it. An Entry whose severity cannot be recognized has no Level.
_Avoid_: Severity, log level

**Search**:
Locating matching text while every Entry stays visible, with jumping between matches.
_Avoid_: Find, query

**Filter**:
A condition that hides every Entry not matching it, narrowing the view to the matching Entries.
_Avoid_: Search, query

**Context**:
The Entries immediately before and after a given Entry. Shown either by temporarily lifting a Filter around one Entry, or by keeping N surrounding Entries visible around each Filter match.
_Avoid_: Surrounding lines, neighbors

**Highlight**:
Rule-based coloring of content (e.g. a level, or a keyword) so it stands out. It does not hide anything.
_Avoid_: Mark, color rule

**Bookmark**:
A manual marker on a single Entry, used to remember a position and jump back to it.
_Avoid_: Pin, flag

**Follow Tail**:
A mode in which the view keeps showing newly appended Entries at the bottom of an open Log File as they are written.
_Avoid_: Live tail, auto-refresh, streaming

**Format Detection**:
Determining a Log File's Format automatically when it is opened. The user can override the result by choosing another built-in Format.
_Avoid_: Format sniffing, auto-parse
