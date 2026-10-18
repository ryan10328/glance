# Glance

A desktop log viewer for opening and reading local log files, whatever their format.

## Language

### Log data

**Log File**:
A local file whose content is log data, regardless of its extension (e.g. `.log` or `.txt`).
_Avoid_: Log, source file

**Format**:
The structure shared by every line of a Log File (e.g. plain text, JSON lines). A Log File has exactly one Format.
_Avoid_: Type, schema

**Format Detection**:
Determining a Log File's Format automatically when it is opened. The user can override the result by choosing another built-in Format.
_Avoid_: Format sniffing, auto-parse

**Entry**:
One logical record in a Log File. It starts at a line carrying a timestamp and includes any following lines without one (e.g. a stack trace). Filtering, selecting and bookmarking operate on Entries.
_Avoid_: Line, record, message

**Level**:
The severity of an Entry, normalized to one of TRACE, DEBUG, INFO, WARN, ERROR, FATAL regardless of how the Format writes it. An Entry whose severity cannot be recognized has no Level.
_Avoid_: Severity, log level

### Reading a Log File

**Tab**:
One open Log File in the window, with its own Filter, Search, Highlight state and Follow Tail setting.
_Avoid_: Document, session

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
Coloring of content so it stands out, applied according to Highlight Rules. It does not hide anything.
_Avoid_: Mark, color rule

**Highlight Rule**:
A user-visible condition (a Level, a keyword or a pattern) paired with a color and a scope, either the whole Entry or only the matching text. Earlier rules in the list take precedence over later ones.
_Avoid_: Color rule, theme

**Bookmark**:
A manual marker on a single Entry, used to remember a position and jump back to it.
_Avoid_: Pin, flag

### Live files

**Follow Tail**:
A mode in which the view keeps showing newly appended Entries at the bottom of an open Log File as they are written. Scrolling up pauses it.
_Avoid_: Live tail, auto-refresh, streaming

**Rotation**:
The open Log File being replaced, renamed or truncated by something else. Glance stops following, tells the user, and reloads only when asked.
_Avoid_: Reset, rollover
