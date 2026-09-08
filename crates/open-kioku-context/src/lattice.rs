//! Identifier lattice: the bridge from task vocabulary to the repository's own identifiers.
//!
//! A task says `CollectionsUtils Tests`; the repository says `CollectionUtilsTests`. Lexical
//! retrieval is substring-based, so it reaches a longer form from a shorter one but never the
//! reverse, and never an identifier whose parts are the task's parts in a different inflection. Between a fifth and a quarter of the
//! remaining holdout misses on a 10k-file Java service and a Python ML library (~4k files) were
//! of this shape.
//!
//! The lattice never invents vocabulary. Every expansion is an identifier that exists in the
//! indexed symbols or file names, whose CamelCase/snake_case parts are the task identifier's
//! parts under a light stem (plural, `-ing`, `-ed`, trailing `e`) or, for parts of six letters
//! or more that the repository does not know at all, a single edit.
//!
//! Only whole identifiers expand. Re-inflecting a task's prose words was measured too: adding
//! the repository's spelling of `batches`, `padded`, or `loading` as search terms floods the
//! lexical stream with generically-named files, and on a Python library (~4k files) it cost
//! 0.021 MRR while gaining nothing on any corpus. Single generic words are weak evidence, the
//! same lesson the task's own lexical terms already encode. Expansions are heuristic links: they feed the lexical stream and the
//! relevance tier, never the exact-symbol stream, so a near miss cannot manufacture exact truth.
//! Built per query from the in-memory symbol and file lists — no index change, no re-index.

use open_kioku_core::{File, Symbol};
use std::collections::HashSet;

/// Repository identifiers per task identifier. Substring matching makes a shorter identifier
/// reach its longer neighbours, so after containment dedupe this is rarely reached.
const MAX_IDENTIFIERS_PER_PROBE: usize = 3;
/// Lattice terms per task: each one is another full lexical pass over the chunk store.
const MAX_TERMS_PER_TASK: usize = 6;
/// Above this many distinct files carrying the reached name, the hop is a common word in
/// identifier form rather than a name, and only widens retrieval.
const MAX_FILES_PER_NAMED_TERM: usize = 4;
/// A single edit is only trusted on parts long enough that one edit is unlikely to land on an
/// unrelated word (`reader` / `header` are one edit apart; so are most five-letter words).
const MIN_EDIT_PART_LEN: usize = 6;
/// Shorter parts are abbreviations (`id`, `str`) whose inflections are not informative.
const MIN_STEM_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LatticeRelation {
    /// Same light stem (`images` / `image`, `CollectionsUtils` / `CollectionUtils`).
    Stem,
    /// One edit apart, and the task's spelling is absent from the repository.
    OneEdit,
}

impl LatticeRelation {
    fn label(self) -> &'static str {
        match self {
            Self::Stem => "stem",
            Self::OneEdit => "one edit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LatticeTerm {
    /// The repository's spelling: an identifier in its original case, or a lowercase part.
    pub term: String,
    /// The task token that reached it.
    pub origin: String,
    pub relation: LatticeRelation,
    /// The name is spread across more files than one edit target can be (`num_frames` is a
    /// parameter in dozens). Such a hop still widens retrieval, but it names nothing in
    /// particular, so it must not confer the named-target tier on every file that has it.
    pub ambiguous: bool,
}

impl LatticeTerm {
    /// Evidence line that says where an expanded term came from, so a reader of the pack can
    /// tell a lattice hop from a word the task actually contains.
    pub fn evidence(&self) -> String {
        format!(
            "identifier lattice: task term `{}` reached repository term `{}` ({})",
            self.origin,
            self.term,
            self.relation.label()
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LatticeExpansion {
    /// Repository identifiers reached from task identifiers, in task order.
    pub identifiers: Vec<LatticeTerm>,
    /// Task identifiers that name nothing in the repository, exactly or through the lattice.
    /// Negative evidence: retrieval for them is running on the task's other words alone.
    pub unreached_identifiers: Vec<String>,
}

#[cfg(test)]
impl LatticeExpansion {
    fn is_empty(&self) -> bool {
        self.identifiers.is_empty()
    }
}

/// Longest identifier part the scan considers; longer ones are hashes or minified blobs.
const MAX_PART_LEN: usize = 40;

/// Part `part` of identifier probe `probe`: where a matching repository part is credited.
#[derive(Debug, Clone, Copy)]
struct Target {
    probe: usize,
    part: usize,
}

/// Probe entries bucketed by byte length, so a repository part is compared only against the
/// handful of probe strings that could equal it (or, for edits, be one edit from it). The
/// scan visits ~180k names on a 10k-file Java index; per-name work must not scale with the
/// number of probes, and a bucket lookup is cheaper than hashing a short string.
struct ProbeTable {
    stems: Vec<Vec<(Vec<u8>, Target)>>,
    edits: Vec<Vec<(Vec<u8>, Target)>>,
    /// Part lengths that can equal a probe stem or word. Most parts (`get`, `id`, `to`)
    /// cannot, and are skipped before being lowercased or stemmed.
    active_lengths: Vec<bool>,
    /// Part lengths within one of an edit probe.
    edit_lengths: Vec<bool>,
    /// First letters of the probe stems and words: a stem keeps its first letter, so a part
    /// starting with any other letter can only match by edit.
    first_letters: [bool; 256],
}

/// The most a stem is shorter than its part (`mappings` → `map`).
const MAX_STEM_SHRINK: usize = 6;

impl ProbeTable {
    fn new() -> Self {
        Self {
            stems: vec![Vec::new(); MAX_PART_LEN + 2],
            edits: vec![Vec::new(); MAX_PART_LEN + 2],
            active_lengths: vec![false; MAX_PART_LEN + 2],
            edit_lengths: vec![false; MAX_PART_LEN + 2],
            first_letters: [false; 256],
        }
    }

    /// Finalises the length filters; returns whether any probe entry exists at all.
    fn seal(&mut self) -> bool {
        let mut any = false;
        for len in 1..=MAX_PART_LEN {
            let stems = (len.saturating_sub(MAX_STEM_SHRINK)..=len)
                .any(|stem_len| !self.stems[stem_len].is_empty());
            let edits = self.edit_candidates(len).next().is_some();
            self.edit_lengths[len] = edits;
            self.active_lengths[len] = stems;
            any |= stems || edits || self.active_lengths[len];
        }
        any
    }

    fn add_stem(&mut self, stem: &[u8], target: Target) {
        if let (Some(first), true) = (stem.first(), stem.len() <= MAX_PART_LEN) {
            self.first_letters[usize::from(*first)] = true;
            self.stems[stem.len()].push((stem.to_vec(), target));
        }
    }

    fn add_edit(&mut self, spelling: &[u8], target: Target) {
        if spelling.len() >= MIN_EDIT_PART_LEN && spelling.len() <= MAX_PART_LEN {
            self.edits[spelling.len()].push((spelling.to_vec(), target));
        }
    }

    fn edit_candidates(&self, len: usize) -> impl Iterator<Item = &(Vec<u8>, Target)> {
        let low = len.saturating_sub(1);
        let high = (len + 1).min(MAX_PART_LEN + 1);
        self.edits[low..=high].iter().flatten()
    }
}

struct IdentifierProbe {
    original: String,
    lower: String,
    /// Some repository identifier already contains the task's spelling, so ordinary substring
    /// retrieval reaches those files and the lattice has nothing to add.
    reachable: bool,
    /// Lowercase parts and their stems, in order.
    parts: Vec<(Vec<u8>, Vec<u8>)>,
    /// Whether the task's spelling of this identifier exists as-is in the repository.
    exact: bool,
    /// Repository identifiers whose parts cover every probe part, with the parts that needed an
    /// edit to match (a bit per probe part).
    matches: Vec<(String, usize, u32)>,
    /// Probe parts that some repository part matched by stem, so an edit is never needed there.
    stem_seen: u32,
}

/// FNV-1a: the name dedupe hashes ~180k short strings per query, where SipHash's
/// per-call setup is most of the cost.
#[derive(Default)]
struct Fnv(u64);

impl std::hash::Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = hash;
    }
}

/// Expand task identifiers and prose words against the identifiers the index knows: symbol
/// names and file stems. Two linear passes over the vocabulary, each distinct name split and
/// stemmed once and matched against the probes through length buckets. The first pass matches
/// by stem only; the second, over the deduplicated names, tries one edit for the probes the
/// first left unreached, which is rare enough that most queries never pay for it.
pub(crate) fn expand(
    task_identifiers: &[String],
    files: &[File],
    symbols: &[Symbol],
) -> LatticeExpansion {
    let mut identifier_probes = Vec::<IdentifierProbe>::new();
    for identifier in task_identifiers
        .iter()
        .filter(|value| is_code_shaped(value))
    {
        let mut parts = Vec::<(Vec<u8>, Vec<u8>)>::new();
        for_each_part(identifier, |part| {
            let lower = part.to_ascii_lowercase().into_bytes();
            let stem = stem_bytes(&lower);
            parts.push((lower, stem));
        });
        // One part is a word, not an identifier: `_slice` would reach every identifier that
        // contains `slice` and hand each of them the named-target tier.
        if parts.len() < 2 || parts.len() > 32 {
            continue;
        }
        identifier_probes.push(IdentifierProbe {
            original: identifier.clone(),
            lower: identifier.to_ascii_lowercase(),
            reachable: false,
            parts,
            exact: false,
            matches: Vec::new(),
            stem_seen: 0,
        });
    }
    if identifier_probes.is_empty() {
        return LatticeExpansion::default();
    }

    let mut table = ProbeTable::new();
    for (index, probe) in identifier_probes.iter().enumerate() {
        for (part, (_, stem)) in probe.parts.iter().enumerate() {
            table.add_stem(stem, Target { probe: index, part });
        }
    }
    table.seal();
    let mut seen = HashSet::<&str, std::hash::BuildHasherDefault<Fnv>>::with_capacity_and_hasher(
        symbols.len() / 2 + files.len(),
        Default::default(),
    );
    let mut scan = Scan::new(identifier_probes.len());
    let file_stems = files.iter().filter_map(|file| file_stem(file));
    for name in symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .chain(file_stems)
    {
        if name.len() > 120 || !name.is_ascii() || !seen.insert(name) {
            continue;
        }
        scan.name(name, &table, &mut identifier_probes);
    }

    // Second pass: an edit is a typo correction, and only trusted for a spelling the
    // repository does not use anywhere. Probes whose every part was seen never enter it.
    let mut edit_table = ProbeTable::new();
    for (index, probe) in identifier_probes.iter().enumerate() {
        let unseen = probe.parts.iter().enumerate().any(|(part, (lower, _))| {
            lower.len() >= MIN_EDIT_PART_LEN && probe.stem_seen & (1 << part) == 0
        });
        if !unseen {
            continue;
        }
        for (part, (lower, stem)) in probe.parts.iter().enumerate() {
            let target = Target { probe: index, part };
            edit_table.add_stem(stem, target);
            edit_table.add_edit(lower, target);
        }
    }
    if edit_table.seal() {
        for name in &seen {
            scan.name(name, &edit_table, &mut identifier_probes);
        }
    }

    let mut expansion = LatticeExpansion::default();
    for probe in identifier_probes {
        // `ImageBackbone` is already inside `ImageBackboneModel`, and `read_frame` inside
        // `read_frame_buffer`: the lexical index reaches both without help. Expanding them only
        // re-tiered every file in the module and cost the true target its lead.
        if probe.exact || probe.reachable {
            continue;
        }
        let mut matches = probe
            .matches
            .into_iter()
            .filter(|(_, _, edited)| edited & probe.stem_seen == 0)
            .collect::<Vec<_>>();
        if matches.is_empty() {
            expansion.unreached_identifiers.push(probe.original);
            continue;
        }
        // Shortest first: substring matching lets `CollectionUtils` reach `CollectionUtilsTests`,
        // so the longer neighbour adds nothing but another pass.
        matches.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then(a.0.len().cmp(&b.0.len()))
                .then(a.0.cmp(&b.0))
        });
        matches.dedup_by(|a, b| a.0 == b.0);
        let mut chosen = Vec::<LatticeTerm>::new();
        for (name, _, edited) in matches {
            let name_lower = name.to_ascii_lowercase();
            if chosen
                .iter()
                .any(|term| name_lower.contains(&term.term.to_ascii_lowercase()))
            {
                continue;
            }
            chosen.push(LatticeTerm {
                term: name,
                origin: probe.original.clone(),
                relation: if edited == 0 {
                    LatticeRelation::Stem
                } else {
                    LatticeRelation::OneEdit
                },
                ambiguous: false,
            });
            if chosen.len() >= MAX_IDENTIFIERS_PER_PROBE {
                break;
            }
        }
        expansion.identifiers.extend(chosen);
    }

    expansion.identifiers.truncate(MAX_TERMS_PER_TASK);
    mark_ambiguous_terms(&mut expansion.identifiers, symbols);
    expansion
}

/// Flag reached names that too many files carry. One pass over the symbol table, comparing
/// only against the handful of chosen terms.
fn mark_ambiguous_terms(terms: &mut [LatticeTerm], symbols: &[Symbol]) {
    if terms.is_empty() {
        return;
    }
    let mut files: Vec<HashSet<&str>> = vec![HashSet::new(); terms.len()];
    for symbol in symbols {
        for (index, term) in terms.iter().enumerate() {
            if symbol.name == term.term && files[index].len() <= MAX_FILES_PER_NAMED_TERM {
                files[index].insert(symbol.file_id.0.as_str());
            }
        }
    }
    for (term, seen) in terms.iter_mut().zip(files) {
        term.ambiguous = seen.len() > MAX_FILES_PER_NAMED_TERM;
    }
}

/// Per-name scratch state: coverage bits per identifier probe and the lowercase and stem
/// buffers, so a pass allocates nothing per name.
struct Scan {
    covered: Vec<u32>,
    edited: Vec<u32>,
    lower: [u8; MAX_PART_LEN],
    stem: [u8; MAX_PART_LEN],
}

impl Scan {
    fn new(identifier_probes: usize) -> Self {
        Self {
            covered: vec![0; identifier_probes],
            edited: vec![0; identifier_probes],
            lower: [0; MAX_PART_LEN],
            stem: [0; MAX_PART_LEN],
        }
    }

    fn name(&mut self, name: &str, table: &ProbeTable, identifier_probes: &mut [IdentifierProbe]) {
        for probe in identifier_probes.iter_mut() {
            if !probe.reachable && contains_ascii_ci(name, probe.lower.as_bytes()) {
                probe.reachable = true;
            }
        }
        let mut touched = false;
        for_each_part_ranges(name, |start, end| {
            let part = &name.as_bytes()[start..end];
            let part_len = part.len();
            if part_len > MAX_PART_LEN {
                return;
            }
            let stem_possible = table.active_lengths[part_len]
                && table.first_letters[usize::from(part[0].to_ascii_lowercase())];
            let edit_possible = table.edit_lengths[part_len];
            if !stem_possible && !edit_possible {
                return;
            }
            for (index, byte) in part.iter().enumerate() {
                self.lower[index] = byte.to_ascii_lowercase();
            }
            let part = &self.lower[..part_len];
            let stem_len = if stem_possible {
                stem_bytes_into(part, &mut self.stem)
            } else {
                0
            };
            let part_stem = &self.stem[..stem_len];

            for (probe_stem, Target { probe, part: bit }) in &table.stems[stem_len] {
                if probe_stem != part_stem {
                    continue;
                }
                self.covered[*probe] |= 1 << bit;
                self.edited[*probe] &= !(1 << bit);
                identifier_probes[*probe].stem_seen |= 1 << bit;
                touched = true;
            }
            if !edit_possible {
                return;
            }
            for (spelling, Target { probe, part: bit }) in table.edit_candidates(part_len) {
                if self.covered[*probe] & (1 << bit) == 0 && within_one_edit_bytes(spelling, part) {
                    self.covered[*probe] |= 1 << bit;
                    self.edited[*probe] |= 1 << bit;
                    touched = true;
                }
            }
        });
        if !touched {
            return;
        }
        let part_count = count_parts(name);
        for (index, probe) in identifier_probes.iter_mut().enumerate() {
            let full = (1u32 << probe.parts.len()) - 1;
            // The repository's spelling of the task's identifier re-inflects its parts; it
            // neither drops them nor adds new ones. `MaxNewTokens` is not
            // `_with_max_new_tokens`, whatever the parts have in common — allowing one spare
            // part let a test helper outrank the module the task was about.
            if self.covered[index] == full && part_count == probe.parts.len() {
                if probe.lower.as_bytes() == name.to_ascii_lowercase().as_bytes() {
                    probe.exact = true;
                }
                probe
                    .matches
                    .push((name.to_string(), part_count, self.edited[index]));
            }
            self.covered[index] = 0;
            self.edited[index] = 0;
        }
    }
}

/// Whether a task token is shaped like source code rather than hyphenated prose. A task's
/// anchors include hyphenated phrases (`right-trimmed`, `kernel-direct`) because a hyphen can
/// separate a real name; re-inflecting their parts against the whole symbol table reaches
/// incidental helpers (`right-trimmed` found a test utility named `right_trim`) and cost the true
/// target its rank. Code names carry an inner case change, an underscore, or digits beside
/// capitals; lower-case hyphenated words carry none of the three.
fn is_code_shaped(value: &str) -> bool {
    value.contains('_')
        || crate::has_inner_case_change(value)
        || (value.chars().any(|ch| ch.is_ascii_digit())
            && value.chars().any(|ch| ch.is_ascii_uppercase()))
}

/// Case-insensitive substring test against a lowercase ASCII needle, without allocating.
fn contains_ascii_ci(haystack: &str, needle: &[u8]) -> bool {
    let haystack = haystack.as_bytes();
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let first = needle[0];
    haystack[..=haystack.len() - needle.len()]
        .iter()
        .enumerate()
        .filter(|(_, byte)| byte.to_ascii_lowercase() == first)
        .any(|(start, _)| {
            haystack[start..start + needle.len()]
                .iter()
                .zip(needle)
                .all(|(byte, want)| byte.to_ascii_lowercase() == *want)
        })
}

fn count_parts(name: &str) -> usize {
    let mut count = 0;
    for_each_part_ranges(name, |_, _| count += 1);
    count
}

fn file_stem(file: &File) -> Option<&str> {
    let name = file.path.file_name()?.to_str()?;
    let stem = name.split('.').find(|segment| !segment.is_empty())?;
    (!stem.is_empty()).then_some(stem)
}

/// Light stemmer for identifier parts. Deliberately not Porter: it strips the inflections that
/// separate a task's word from the repository's (`images` / `image`, `merges` / `merging` /
/// `merged`, `entries` / `entry`) and nothing else. Both sides go through it, so the only cost
/// of over-stemming is an occasional spurious mate, which the per-word cap bounds.
#[cfg(test)]
fn stem(part: &str) -> String {
    String::from_utf8(stem_bytes(part.as_bytes())).unwrap_or_else(|_| part.to_string())
}

fn stem_bytes(part: &[u8]) -> Vec<u8> {
    let mut out = [0u8; MAX_PART_LEN];
    if part.len() > MAX_PART_LEN {
        return part.to_vec();
    }
    let len = stem_bytes_into(part, &mut out);
    out[..len].to_vec()
}

/// Writes the stem of a lowercase ASCII `part` into `out` and returns its length. Only ever
/// shrinks or rewrites in place, so `out` needs no more room than `part`.
fn stem_bytes_into(part: &[u8], out: &mut [u8; MAX_PART_LEN]) -> usize {
    let mut len = part.len();
    out[..len].copy_from_slice(part);
    if len < MIN_STEM_LEN || !part.is_ascii() {
        return len;
    }
    let ends_with = |out: &[u8], len: usize, suffix: &[u8]| {
        len >= suffix.len() && &out[len - suffix.len()..len] == suffix
    };
    // Plural.
    if ends_with(out, len, b"ies") && len > 4 {
        len -= 3;
        out[len] = b'y';
        len += 1;
    } else if ends_with(out, len, b"sses")
        || ends_with(out, len, b"xes")
        || ends_with(out, len, b"zes")
        || ends_with(out, len, b"ches")
        || ends_with(out, len, b"shes")
    {
        len -= 2;
    } else if ends_with(out, len, b"s")
        && !(ends_with(out, len, b"ss") || ends_with(out, len, b"us") || ends_with(out, len, b"is"))
    {
        len -= 1;
    }
    // Verb suffixes; the remainder must still be a word (`string` is not `str` + `ing`).
    if ends_with(out, len, b"ing") && len - 3 >= 4 {
        len -= 3;
        len = collapse_double_consonant(out, len);
    } else if ends_with(out, len, b"ed") && len - 2 >= 4 {
        len -= 2;
        len = collapse_double_consonant(out, len);
    }
    // Trailing `e`, so `merge`, `merged`, and `merging` agree.
    if ends_with(out, len, b"e") && len >= 4 {
        len -= 1;
    }
    len
}

fn collapse_double_consonant(out: &[u8], len: usize) -> usize {
    if len >= 2
        && out[len - 1] == out[len - 2]
        && !matches!(
            out[len - 1],
            b'a' | b'e' | b'i' | b'o' | b'u' | b'l' | b's' | b'z'
        )
    {
        len - 1
    } else {
        len
    }
}

/// Optimal-string-alignment distance of exactly one: a substitution, an insertion, a deletion,
/// or a transposition of adjacent characters. Byte-wise; callers pass lowercase ASCII.
#[cfg(test)]
fn within_one_edit(a: &str, b: &str) -> bool {
    within_one_edit_bytes(a.as_bytes(), b.as_bytes())
}

fn within_one_edit_bytes(a: &[u8], b: &[u8]) -> bool {
    if a == b {
        return false;
    }
    match a.len().abs_diff(b.len()) {
        0 => {
            let mut mismatches = [0usize; 2];
            let mut count = 0;
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                if x != y {
                    if count == 2 {
                        return false;
                    }
                    mismatches[count] = index;
                    count += 1;
                }
            }
            match count {
                1 => true,
                2 => {
                    let [first, second] = mismatches;
                    second == first + 1 && a[first] == b[second] && a[second] == b[first]
                }
                _ => false,
            }
        }
        1 => {
            let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
            let split = short
                .iter()
                .zip(long)
                .position(|(x, y)| x != y)
                .unwrap_or(short.len());
            short[split..] == long[split + 1..]
        }
        _ => false,
    }
}

/// CamelCase and snake_case parts of an identifier, as slices of the original. Mirrors the
/// index tokenizer's boundaries: a lower-case letter or digit followed by an upper-case letter
/// (`fieldMapper`), and the last upper-case letter of a run before a lower-case letter
/// (`HTTPServer` is `HTTP`, `Server`).
fn for_each_part(value: &str, mut visit: impl FnMut(&str)) {
    for_each_part_ranges(value, |start, end| visit(&value[start..end]));
}

fn for_each_part_ranges(value: &str, mut visit: impl FnMut(usize, usize)) {
    let bytes = value.as_bytes();
    let mut start: Option<usize> = None;
    for (index, &byte) in bytes.iter().enumerate() {
        if !byte.is_ascii_alphanumeric() {
            if let Some(begin) = start.take() {
                if index - begin >= 2 {
                    visit(begin, index);
                }
            }
            continue;
        }
        let Some(begin) = start else {
            start = Some(index);
            continue;
        };
        let prev = bytes[index - 1];
        let next = bytes.get(index + 1).copied();
        let starts_new_word = byte.is_ascii_uppercase()
            && (prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next.is_some_and(|n| n.is_ascii_lowercase())));
        if starts_new_word {
            if index - begin >= 2 {
                visit(begin, index);
            }
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        if bytes.len() - begin >= 2 {
            visit(begin, bytes.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EvidenceSourceType, FileId, Language, RepositoryId, SymbolId, SymbolKind,
    };

    fn file(path: &str) -> File {
        File {
            id: FileId::new(path),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::Java,
            size_bytes: 1,
            content_hash: path.into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn symbol(name: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(name),
            name: name.into(),
            qualified_name: name.into(),
            kind: SymbolKind::Class,
            file_id: FileId::new("f"),
            range: None,
            language: Language::Java,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn stems_plurals_and_verb_suffixes_symmetrically() {
        for group in [
            &["image", "images"][..],
            &["merge", "merges", "merged", "merging"],
            &["entry", "entries"],
            &["query", "queries"],
            &["class", "classes"],
            &["match", "matches"],
            &["index", "indexes", "indexed"],
            &["setting", "settings"],
            &["mapping", "mappings", "mapped"],
            &["collection", "collections"],
            &["util", "utils"],
            &["token", "tokens"],
        ] {
            let stems = group.iter().map(|word| stem(word)).collect::<Vec<_>>();
            assert!(
                stems.iter().all(|value| value == &stems[0]),
                "{group:?} stemmed to {stems:?}"
            );
        }
    }

    #[test]
    fn stemmer_leaves_short_words_and_false_suffixes_alone() {
        assert_eq!(stem("string"), "string");
        assert_eq!(stem("status"), "status");
        assert_eq!(stem("analysis"), "analysis");
        assert_eq!(stem("class"), "class");
        assert_eq!(stem("need"), "need");
        assert_eq!(stem("this"), "this");
        assert_eq!(stem("was"), "was");
        assert_ne!(stem("string"), stem("str"));
    }

    #[test]
    fn one_edit_covers_substitution_insertion_deletion_and_transposition() {
        assert!(within_one_edit("collection", "colection"));
        assert!(within_one_edit("collection", "collectoin"));
        assert!(within_one_edit("collection", "collectionn"));
        assert!(within_one_edit("collection", "cellection"));
        assert!(!within_one_edit("collection", "collection"));
        assert!(!within_one_edit("collection", "colectoin"));
        assert!(!within_one_edit("collection", "collect"));
    }

    #[test]
    fn splits_camel_snake_and_acronyms_like_the_index_tokenizer() {
        let mut parts = Vec::new();
        for_each_part("HTTPServerConfig_v2", |part| parts.push(part.to_string()));
        assert_eq!(parts, strings(&["HTTP", "Server", "Config", "v2"]));
        parts.clear();
        for_each_part("max_new_tokens", |part| parts.push(part.to_string()));
        assert_eq!(parts, strings(&["max", "new", "tokens"]));
    }

    #[test]
    fn misinflected_identifier_reaches_the_repository_spelling() {
        let symbols = vec![
            symbol("CollectionUtils"),
            symbol("CollectionUtilsTests"),
            symbol("ArrayUtils"),
        ];
        let files = vec![file("server/src/test/java/util/CollectionUtilsTests.java")];
        let expansion = expand(&strings(&["CollectionsUtils"]), &files, &symbols);
        // The shorter identifier reaches the longer one by substring; only it is added.
        assert_eq!(
            expansion
                .identifiers
                .iter()
                .map(|term| term.term.as_str())
                .collect::<Vec<_>>(),
            vec!["CollectionUtils"]
        );
        assert_eq!(expansion.identifiers[0].origin, "CollectionsUtils");
        assert_eq!(expansion.identifiers[0].relation, LatticeRelation::Stem);
        assert!(expansion.unreached_identifiers.is_empty());
    }

    #[test]
    fn one_part_identifiers_and_part_supersets_are_not_reached() {
        let symbols = vec![
            symbol("SliceBuilder"),
            symbol("testByteSlicingArray"),
            symbol("NearestVectorFieldValues"),
            symbol("NearestVectorValuesTests"),
        ];
        // `_slice` is a word with a separator, not an identifier with parts to re-inflect.
        let word = expand(&strings(&["_slice"]), &[], &symbols);
        assert!(word.is_empty());
        assert!(word.unreached_identifiers.is_empty());
        // `NearestVectorValues` sits inside `NearestVectorValuesTests`, which substring retrieval
        // already reaches; and the five-part field is not the same identifier in any case.
        let superset = expand(&strings(&["NearestVectorValues"]), &[], &symbols);
        assert!(superset.is_empty(), "{superset:?}");
        // With only the longer, differently-inflected field present, the part-count cap still
        // rejects it: three parts do not become five.
        let capped = expand(
            &strings(&["NearestVectorValue"]),
            &[],
            &[symbol("NearestVectorScriptFieldValuesTests")],
        );
        assert!(capped.identifiers.is_empty(), "{capped:?}");
    }

    #[test]
    fn an_identifier_substring_search_already_reaches_is_not_expanded() {
        // Every file that matters mentions `ImageBackboneModel`, and plain substring retrieval
        // finds them from `ImageBackbone`; expanding would only re-tier the whole module.
        let symbols = vec![
            symbol("ImageBackbone"),
            symbol("ImageBackboneModel"),
            symbol("ImageBackboneConfig"),
            symbol("read_frame_buffer"),
        ];
        for probe in ["ImageBackbone", "read_frame"] {
            let expansion = expand(&strings(&[probe]), &[], &symbols);
            assert!(expansion.is_empty(), "{probe}: {expansion:?}");
            assert!(expansion.unreached_identifiers.is_empty());
        }
        // The misspelling is a substring of nothing, so it is still expanded — to the
        // identifier whose parts it re-inflects, not to the longer names built on it.
        let typo = expand(&strings(&["ImageBackbones"]), &[], &symbols);
        assert_eq!(
            typo.identifiers
                .iter()
                .map(|term| term.term.as_str())
                .collect::<Vec<_>>(),
            vec!["ImageBackbone"]
        );
    }

    #[test]
    fn hyphenated_prose_is_not_treated_as_an_identifier() {
        let symbols = vec![symbol("right_trim"), symbol("CollectionUtils")];
        let prose = expand(&strings(&["right-trimmed", "kernel-direct"]), &[], &symbols);
        assert!(prose.is_empty(), "{prose:?}");
        // Nor is it reported as missing vocabulary: it was never a code name to look for.
        assert!(prose.unreached_identifiers.is_empty());
        // A snake_case or CamelCase token still is one.
        assert!(!expand(&strings(&["right_trimmed"]), &[], &symbols)
            .identifiers
            .is_empty());
    }

    #[test]
    fn exact_identifier_is_not_expanded() {
        let symbols = vec![symbol("CollectionUtils"), symbol("CollectionUtilsTests")];
        let expansion = expand(&strings(&["CollectionUtils"]), &[], &symbols);
        assert!(expansion.is_empty());
        assert!(expansion.unreached_identifiers.is_empty());
    }

    #[test]
    fn typo_in_a_long_part_is_corrected_only_when_the_repository_lacks_that_spelling() {
        let symbols = vec![symbol("ReaderUtils"), symbol("HeaderUtils")];
        // `Header` exists, so `HeaderUtils` is exact and nothing is expanded.
        let exact = expand(&strings(&["HeaderUtils"]), &[], &symbols);
        assert!(exact.is_empty());
        // `Haeder` exists nowhere: one transposition reaches `Header`; `Reader` is three
        // edits away and is not offered.
        let typo = expand(&strings(&["HaederUtils"]), &[], &symbols);
        let names = typo
            .identifiers
            .iter()
            .map(|term| term.term.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["HeaderUtils"]);
        assert!(typo
            .identifiers
            .iter()
            .all(|term| term.relation == LatticeRelation::OneEdit));
        // `Readr` is a five-letter part: too short for an edit to be trusted.
        let short = expand(&strings(&["ReadrUtils"]), &[], &symbols);
        assert!(short.identifiers.is_empty());
        assert_eq!(short.unreached_identifiers, strings(&["ReadrUtils"]));
    }

    #[test]
    fn a_name_many_files_carry_is_kept_but_flagged_ambiguous() {
        // `num_frames` is a parameter across a whole package: reaching it widens retrieval,
        // but it names no single edit target, so it must not confer the named-target tier.
        let mut symbols = (0..6)
            .map(|index| {
                let mut sym = symbol("num_frames");
                sym.id = SymbolId::new(format!("num_frames-{index}"));
                sym.file_id = FileId::new(format!("file-{index}"));
                sym
            })
            .collect::<Vec<_>>();
        symbols.push(symbol("CollectionUtils"));
        let expansion = expand(&strings(&["NumFrames", "CollectionsUtils"]), &[], &symbols);
        let flags = expansion
            .identifiers
            .iter()
            .map(|term| (term.term.as_str(), term.ambiguous))
            .collect::<Vec<_>>();
        assert_eq!(
            flags,
            vec![("num_frames", true), ("CollectionUtils", false)]
        );
    }

    #[test]
    fn unreached_identifier_is_reported_as_negative_evidence() {
        let symbols = vec![symbol("CollectionUtils")];
        let expansion = expand(&strings(&["QuantumFluxCapacitor"]), &[], &symbols);
        assert!(expansion.is_empty());
        assert_eq!(
            expansion.unreached_identifiers,
            strings(&["QuantumFluxCapacitor"])
        );
    }

    #[test]
    fn file_stems_count_as_vocabulary() {
        let files = vec![file("src/processors/image_loaders.py")];
        let expansion = expand(&strings(&["ImageLoader"]), &files, &[]);
        assert_eq!(expansion.identifiers.len(), 1);
        assert_eq!(expansion.identifiers[0].term, "image_loaders");
    }

    #[test]
    fn expansion_is_bounded_per_task() {
        let symbols = (0..20)
            .map(|index| symbol(&format!("Widget{index}Handler")))
            .collect::<Vec<_>>();
        let identifiers = (0..10)
            .map(|index| format!("Widgets{index}Handlers"))
            .collect::<Vec<_>>();
        let expansion = expand(&identifiers, &[], &symbols);
        assert!(expansion.identifiers.len() <= MAX_TERMS_PER_TASK);
    }
}
