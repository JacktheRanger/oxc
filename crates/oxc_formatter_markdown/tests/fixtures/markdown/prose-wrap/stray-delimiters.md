<!-- Normalizations that would pair a marker with a stray run are skipped:
     a code span fence never equals a literal backtick run before it,
     `__strong__` stays when a literal `*` precedes it, `*em*` stays when a literal `_` precedes it -->

A stray ` backtick and then ``code`` that must not shrink to one backtick.

A literal *** run and then __strong__ that must not become two asterisks.

A literal _ underscore and then *emphasis* that must not become underscores.

No stray run here, so ``code`` shrinks and __strong__ and *emphasis* normalize.

A backslash \ before a line break stays on its line, never a hard break \
when the next word arrives.

[label]: /destination followed by text that is not a title keeps its first line
whole (a wrap after the destination would leave a definition behind).

text\
	:::note after a hard break was indented in the source and stays paragraph text
