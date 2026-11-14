### Ripple

- **6 site(s) no longer fit what this change altered** (3 changed declaration(s); 1 other call(s) fit).
  - `tests/test\_black.py:2663` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2673` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2682` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2694` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2743` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]
  - `tests/test\_black.py:2773` does not pass \`cache\_dir\`, which is required now — `src/black/cache.py: get\_cache\_file` [proven]

