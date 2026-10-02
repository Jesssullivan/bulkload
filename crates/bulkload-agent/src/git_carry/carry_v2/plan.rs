//! The object list, its segments, and `<pack_id>.list`.

use super::super::estimate::Refused;
use super::negotiate::shallow_shape;
use super::{is_oid, lines, pinned, run_child, FirstRound, Outcome, Source};
use crate::BulkloadRefusal;

/// Stored bytes (`%(objectsize:disk)`) per segment: about 64 MiB.
///
/// This is the plan's D2 figure. The cap is soft: it is measured on the source's stored size,
/// and a delta whose base falls in another segment is recomputed (spike Q2).
/// An object larger than the cap gets a segment of its own.
pub const DEFAULT_SEGMENT_CAP: u64 = 64 << 20;

/// First line of a `<pack_id>.list`.
const HEADER: &[u8] = b"bulkload-git-carry-list v1";

/// A first round's object list, cut into self-contained thin segments.
///
/// Every line is raw bytes as `rev-list` printed it: `-<oid>` for an edge (a
/// preferred base), `<oid>` or `<oid> <path>` for an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackPlan {
    pack_id: String,
    wants: Vec<String>,
    haves: Vec<String>,
    shallow: Vec<String>,
    cap: u64,
    edges: Vec<Vec<u8>>,
    segments: Vec<Vec<Vec<u8>>>,
}

impl PackPlan {
    /// List what `round` sends from `source` and cut it into segments of at
    /// most `cap` stored bytes (at least one object each).
    ///
    /// # Errors
    /// Refuses a walk that reaches an object the source lacks
    /// (`source_lacks_reachable_objects`), a zero cap, and Git output of the
    /// wrong shape.
    pub fn build(
        source: &Source,
        round: &FirstRound,
        cap: u64,
        store: Option<&super::StderrStore>,
    ) -> Outcome<Self> {
        if cap == 0 {
            return Err(BulkloadRefusal::FieldDomainViolation.into());
        }
        // #73 round-2 D1: the round's destination frontier must still fit
        // this source (R-N75, R-N131), whoever built the round and however
        // the source moved since.
        shallow_shape(&source.shallow, &round.shallow().iter().cloned().collect())?;
        let (edges, objects) = object_list(source, round, store)?;
        let sized = sizes(source, &objects, store)?;
        let segments = cut(objects, &sized, cap);
        Ok(Self::sealed(
            round.wants().to_vec(),
            round.haves().to_vec(),
            round.shallow().to_vec(),
            cap,
            edges,
            segments,
        ))
    }

    fn sealed(
        wants: Vec<String>,
        haves: Vec<String>,
        shallow: Vec<String>,
        cap: u64,
        edges: Vec<Vec<u8>>,
        segments: Vec<Vec<Vec<u8>>>,
    ) -> Self {
        let mut plan = Self {
            pack_id: String::new(),
            wants,
            haves,
            shallow,
            cap,
            edges,
            segments,
        };
        plan.pack_id = blake3::hash(&plan.encode()).to_hex().to_string();
        plan
    }

    /// The BLAKE3 of the encoded list: the name `GitResume` carries.
    #[must_use]
    pub fn pack_id(&self) -> &str {
        &self.pack_id
    }

    /// Segments to send; zero when the destination already holds everything
    /// (then no pack is sent at all).
    #[must_use]
    pub const fn segments(&self) -> usize {
        self.segments.len()
    }

    /// Objects in segment `index`.
    #[must_use]
    pub fn segment_objects(&self, index: usize) -> Option<usize> {
        self.segments.get(index).map(Vec::len)
    }

    /// Objects across every segment.
    #[must_use]
    pub fn objects(&self) -> usize {
        self.segments.iter().map(Vec::len).sum()
    }

    /// Edge (preferred-base) lines.
    #[must_use]
    pub const fn edges(&self) -> usize {
        self.edges.len()
    }

    /// The haves this plan was built from, in the order offered.
    #[must_use]
    pub fn haves(&self) -> &[String] {
        &self.haves
    }

    /// The wants this plan was built from.
    #[must_use]
    pub fn wants(&self) -> &[String] {
        &self.wants
    }

    /// The destination frontier this plan was built for (empty for a full
    /// destination).
    #[must_use]
    pub fn shallow(&self) -> &[String] {
        &self.shallow
    }

    /// Refuse unless `source`'s frontier fits this plan's destination
    /// frontier (R-N75, R-N131). [`PackPlan::send_segment`] runs it on
    /// every send, so a plan loaded for resume is checked against the
    /// source as it is now (#73 round-2 D1).
    ///
    /// # Errors
    /// `GIT_HAVES_UNPROVABLE` with the rule's reason.
    pub fn check_source(&self, source: &Source) -> Outcome<()> {
        shallow_shape(&source.shallow, &self.shallow.iter().cloned().collect())
    }

    /// The segment cap, in stored bytes.
    #[must_use]
    pub const fn cap(&self) -> u64 {
        self.cap
    }

    /// `pack-objects` stdin for segment `index`: every edge line, then the
    /// segment's object lines.
    pub(super) fn segment_input(&self, index: usize) -> Option<Vec<u8>> {
        let segment = self.segments.get(index)?;
        let mut input = Vec::new();
        for line in self.edges.iter().chain(segment) {
            input.extend_from_slice(line);
            input.push(b'\n');
        }
        Some(input)
    }

    /// The `<pack_id>.list` bytes:
    ///
    /// ```text
    /// bulkload-git-carry-list v1
    /// want <oid>        (each, sorted)
    /// have <oid>        (each, in offered order)
    /// shallow <oid>     (each, sorted)
    /// cap <bytes>
    /// -<oid>            (each edge)
    /// segment <k>       (k = 0, 1, ...; then that segment's object lines)
    /// end
    /// ```
    ///
    /// Object lines are raw bytes: 40 or 64 hex digits, then nothing or a
    /// space and the path. No marker line has that shape (`cap ` and `end`
    /// start with hex digits but break it by their fourth byte), so none can
    /// be mistaken for an object.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut line = |parts: &[&[u8]]| {
            for part in parts {
                out.extend_from_slice(part);
            }
            out.push(b'\n');
        };
        line(&[HEADER]);
        for value in &self.wants {
            line(&[b"want ", value.as_bytes()]);
        }
        for value in &self.haves {
            line(&[b"have ", value.as_bytes()]);
        }
        for value in &self.shallow {
            line(&[b"shallow ", value.as_bytes()]);
        }
        line(&[b"cap ", self.cap.to_string().as_bytes()]);
        for edge in &self.edges {
            line(&[edge]);
        }
        for (index, segment) in self.segments.iter().enumerate() {
            line(&[b"segment ", index.to_string().as_bytes()]);
            for object in segment {
                line(&[object]);
            }
        }
        line(&[b"end"]);
        out
    }

    /// Parse `<pack_id>.list` bytes written by [`PackPlan::encode`].
    ///
    /// # Errors
    /// Refuses `SCHEMA_MISMATCH` for any byte sequence `encode` could not
    /// have written.
    pub fn decode(bytes: &[u8]) -> crate::Result<Self> {
        let bad = || BulkloadRefusal::SchemaMismatch;
        let body = bytes.strip_suffix(b"\n").ok_or_else(bad)?;
        let mut rows = body.split(|b| *b == b'\n');
        if rows.next() != Some(HEADER) {
            return Err(bad());
        }
        let text = |value: &[u8]| -> crate::Result<String> {
            if !is_oid(value) {
                return Err(bad());
            }
            String::from_utf8(value.to_vec()).map_err(|_| bad())
        };
        let (mut wants, mut haves, mut shallow) = (Vec::new(), Vec::new(), Vec::new());
        let mut cap = None;
        let mut edges = Vec::new();
        let mut segments: Vec<Vec<Vec<u8>>> = Vec::new();
        let mut ended = false;
        for row in rows {
            if ended {
                return Err(bad());
            }
            // Sections come in the order `encode` writes them.
            let stage = usize::from(cap.is_some()) + usize::from(!segments.is_empty());
            if let Some(value) = row.strip_prefix(b"want ") {
                if stage > 0 || !haves.is_empty() || !shallow.is_empty() {
                    return Err(bad());
                }
                wants.push(text(value)?);
            } else if let Some(value) = row.strip_prefix(b"have ") {
                if stage > 0 || !shallow.is_empty() {
                    return Err(bad());
                }
                haves.push(text(value)?);
            } else if let Some(value) = row.strip_prefix(b"shallow ") {
                if stage > 0 {
                    return Err(bad());
                }
                shallow.push(text(value)?);
            } else if let Some(value) = row.strip_prefix(b"cap ") {
                if stage > 0 {
                    return Err(bad());
                }
                let value = std::str::from_utf8(value).map_err(|_| bad())?;
                let parsed = value.parse::<u64>().map_err(|_| bad())?;
                if parsed == 0 || parsed.to_string() != value {
                    return Err(bad());
                }
                cap = Some(parsed);
            } else if let Some(value) = row.strip_prefix(b"-") {
                if stage != 1 || !is_oid(value) {
                    return Err(bad());
                }
                edges.push(row.to_vec());
            } else if let Some(value) = row.strip_prefix(b"segment ") {
                if stage == 0 || segments.last().is_some_and(Vec::is_empty) {
                    return Err(bad());
                }
                if value != segments.len().to_string().as_bytes() {
                    return Err(bad());
                }
                segments.push(Vec::new());
            } else if row == b"end" {
                if stage == 0 || segments.last().is_some_and(Vec::is_empty) {
                    return Err(bad());
                }
                ended = true;
            } else {
                let segment = segments.last_mut().ok_or_else(bad)?;
                if !object_line(row) {
                    return Err(bad());
                }
                segment.push(row.to_vec());
            }
        }
        if !ended {
            return Err(bad());
        }
        let plan = Self::sealed(wants, haves, shallow, cap.ok_or_else(bad)?, edges, segments);
        // Canonical form only: decoding what `encode` wrote gives it back.
        if plan.encode() != bytes {
            return Err(bad());
        }
        Ok(plan)
    }
}

/// `<oid>` or `<oid> <path bytes>`, as `rev-list --objects` prints an object.
fn object_line(line: &[u8]) -> bool {
    let (name, rest) = line
        .iter()
        .position(|b| *b == b' ')
        .map_or((line, None), |at| {
            (line.get(..at).unwrap_or_default(), line.get(at..))
        });
    is_oid(name) && rest.is_none_or(|rest| !rest.contains(&b'\n'))
}

/// Edge lines and object lines.
type Listed = (Vec<Vec<u8>>, Vec<Vec<u8>>);

/// `rev-list --objects-edge[-aggressive] --missing=print --stdin` over the
/// round's request: (edge lines, object lines). Any `?<oid>` line (an object
/// the walk needs and the source lacks) refuses.
fn object_list(
    source: &Source,
    round: &FirstRound,
    store: Option<&super::StderrStore>,
) -> Outcome<Listed> {
    let mut input = Vec::new();
    for value in round.wants() {
        input.extend_from_slice(value.as_bytes());
        input.push(b'\n');
    }
    if round.wants().is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    if !round.haves().is_empty() {
        input.extend_from_slice(b"--not\n");
        for value in round.haves() {
            input.extend_from_slice(value.as_bytes());
            input.push(b'\n');
        }
    }
    let edge = if round.shallow_destination() {
        "--objects-edge-aggressive"
    } else {
        "--objects-edge"
    };
    let mut edges = Vec::new();
    let mut objects = Vec::new();
    let mut unavailable = false;
    run_child(
        pinned(source).args(["rev-list", edge, "--missing=print", "--stdin"]),
        &input,
        store,
        "object_list_failed",
        |stdout| {
            lines(stdout, |line| {
                if let Some(value) = line.strip_prefix(b"?") {
                    if !is_oid(value) {
                        return Err(BulkloadRefusal::GitInventoryMalformed);
                    }
                    unavailable = true;
                } else if let Some(value) = line.strip_prefix(b"-") {
                    if !is_oid(value) {
                        return Err(BulkloadRefusal::GitInventoryMalformed);
                    }
                    edges.push(line.to_vec());
                } else if object_line(line) {
                    objects.push(line.to_vec());
                } else {
                    return Err(BulkloadRefusal::GitInventoryMalformed);
                }
                Ok(())
            })
        },
    )?;
    if unavailable {
        return Err(Refused::because(
            BulkloadRefusal::GitInventoryMalformed,
            "source_lacks_reachable_objects",
        ));
    }
    if objects.is_empty() {
        edges.clear();
    }
    Ok((edges, objects))
}

/// Order key and stored size of one listed object.
struct Sized {
    rank: u8,
    disk: u64,
}

/// `cat-file --batch-check` type and `%(objectsize:disk)` of every object,
/// in list order.
fn sizes(
    source: &Source,
    objects: &[Vec<u8>],
    store: Option<&super::StderrStore>,
) -> Outcome<Vec<Sized>> {
    if objects.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = Vec::with_capacity(objects.len() * 65);
    for line in objects {
        input.extend_from_slice(oid_of(line));
        input.push(b'\n');
    }
    let mut sized = Vec::with_capacity(objects.len());
    run_child(
        pinned(source).args([
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize:disk)",
        ]),
        &input,
        store,
        "object_sizes_failed",
        |stdout| {
            lines(stdout, |line| {
                let mut fields = line.split(|b| *b == b' ');
                let (Some(name), Some(kind), Some(disk), None) =
                    (fields.next(), fields.next(), fields.next(), fields.next())
                else {
                    return Err(BulkloadRefusal::GitInventoryMalformed);
                };
                let expected = objects
                    .get(sized.len())
                    .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
                if name != oid_of(expected) {
                    return Err(BulkloadRefusal::GitInventoryMalformed);
                }
                let rank = match kind {
                    b"commit" => 0,
                    b"tag" => 1,
                    b"tree" => 2,
                    b"blob" => 3,
                    _ => return Err(BulkloadRefusal::GitInventoryMalformed),
                };
                let disk = std::str::from_utf8(disk)
                    .ok()
                    .and_then(|disk| disk.parse::<u64>().ok())
                    .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
                sized.push(Sized { rank, disk });
                Ok(())
            })
        },
    )?;
    if sized.len() != objects.len() {
        return Err(BulkloadRefusal::GitInventoryMalformed.into());
    }
    Ok(sized)
}

fn oid_of(line: &[u8]) -> &[u8] {
    line.split(|b| *b == b' ').next().unwrap_or(line)
}

/// The path after an object line's oid (empty for a commit or a root tree).
fn path_of(line: &[u8]) -> &[u8] {
    line.iter()
        .position(|b| *b == b' ')
        .and_then(|at| line.get(at + 1..))
        .unwrap_or_default()
}

fn basename_of(path: &[u8]) -> &[u8] {
    path.rsplit(|b| *b == b'/').next().unwrap_or(path)
}

/// Assign objects to segments of at most `cap` stored bytes in
/// (type, basename, path) order, then give each segment its objects in list
/// (rev-list) order. With everything under the cap there is one segment,
/// holding the list exactly as `rev-list` printed it.
fn cut(objects: Vec<Vec<u8>>, sized: &[Sized], cap: u64) -> Vec<Vec<Vec<u8>>> {
    let mut order: Vec<usize> = (0..objects.len()).collect();
    order.sort_by(|&a, &b| {
        let key = |at: usize| {
            let line = objects.get(at).map_or(&[][..], Vec::as_slice);
            let path = path_of(line);
            (
                sized.get(at).map_or(u8::MAX, |s| s.rank),
                basename_of(path),
                path,
                at,
            )
        };
        key(a).cmp(&key(b))
    });
    let mut assigned = vec![0_usize; objects.len()];
    let mut segment = 0_usize;
    let mut size = 0_u64;
    let mut first = true;
    for at in order {
        let disk = sized.get(at).map_or(0, |s| s.disk);
        if !first && size.saturating_add(disk) > cap {
            segment += 1;
            size = 0;
        }
        first = false;
        size = size.saturating_add(disk);
        if let Some(slot) = assigned.get_mut(at) {
            *slot = segment;
        }
    }
    let count = if objects.is_empty() { 0 } else { segment + 1 };
    let mut segments: Vec<Vec<Vec<u8>>> = vec![Vec::new(); count];
    for (line, index) in objects.into_iter().zip(assigned) {
        if let Some(target) = segments.get_mut(index) {
            target.push(line);
        }
    }
    segments
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn oid(n: u8) -> String {
        format!("{n:02x}").repeat(20)
    }

    fn sample() -> PackPlan {
        PackPlan::sealed(
            vec![oid(1)],
            vec![oid(3), oid(2)],
            vec![],
            4096,
            vec![format!("-{}", oid(3)).into_bytes()],
            vec![
                vec![oid(1).into_bytes(), format!("{} ", oid(4)).into_bytes()],
                vec![[oid(5).as_bytes(), b" d\xff/\x01 x"].concat()],
            ],
        )
    }

    #[test]
    fn a_list_round_trips_and_is_named_by_its_digest() {
        let plan = sample();
        let bytes = plan.encode();
        assert_eq!(plan.pack_id(), blake3::hash(&bytes).to_hex().as_str());
        let decoded = PackPlan::decode(&bytes).unwrap();
        assert_eq!(decoded, plan);
        assert_eq!(decoded.segments(), 2);
        assert_eq!(decoded.objects(), 3);
    }

    #[test]
    fn a_list_that_encode_could_not_write_is_refused() {
        let bytes = sample().encode();
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        let mutations: Vec<Vec<u8>> = vec![
            bytes[..bytes.len() - 1].to_vec(),
            [&bytes[..], b"x\n"].concat(),
            text(&bytes).replace("segment 1", "segment 2").into_bytes(),
            text(&bytes).replace("cap 4096", "cap 0").into_bytes(),
            text(&bytes).replace("cap 4096", "cap 04096").into_bytes(),
            text(&bytes)
                .replace("bulkload-git-carry-list v1", "bulkload-git-carry-list v2")
                .into_bytes(),
            text(&bytes).replace("\nend\n", "\n").into_bytes(),
            text(&bytes)
                .replace(
                    &format!("have {}\nhave", oid(3)),
                    &format!("have {}\nwant", oid(3)),
                )
                .into_bytes(),
            text(&bytes)
                .replace(
                    &format!("segment 0\n{}", oid(1)),
                    &format!("segment 0\nzz{}", &oid(1)[2..]),
                )
                .into_bytes(),
            text(&bytes)
                .replace("segment 1\n", "segment 1\nsegment 2\n")
                .into_bytes(),
        ];
        for (index, mutated) in mutations.iter().enumerate() {
            assert_ne!(mutated, &bytes, "mutation {index} is a no-op");
            assert_eq!(
                PackPlan::decode(mutated),
                Err(BulkloadRefusal::SchemaMismatch),
                "mutation {index}"
            );
        }
    }

    #[test]
    fn one_segment_keeps_list_order_and_the_cap_groups_by_basename() {
        let lines: Vec<Vec<u8>> = [
            oid(1),
            format!("{} ", oid(2)),
            format!("{} a/x.txt", oid(3)),
            format!("{} b/y.txt", oid(4)),
            format!("{} c/x.txt", oid(5)),
        ]
        .into_iter()
        .map(String::into_bytes)
        .collect();
        let sized = [(0, 10), (2, 10), (3, 100), (3, 100), (3, 100)]
            .map(|(rank, disk)| Sized { rank, disk });
        assert_eq!(cut(lines.clone(), &sized, 1_000), vec![lines.clone()]);
        // Cap 220: the commit, the root tree and both x.txt versions (one
        // basename across two directories) fill the first segment, in list
        // order; b/y.txt starts the second.
        let segments = cut(lines.clone(), &sized, 220);
        assert_eq!(
            segments,
            vec![
                vec![
                    lines[0].clone(),
                    lines[1].clone(),
                    lines[2].clone(),
                    lines[4].clone()
                ],
                vec![lines[3].clone()],
            ]
        );
        // An object over the cap still gets a segment.
        assert_eq!(cut(lines, &sized, 1).len(), 5);
        assert!(cut(Vec::new(), &[], 1).is_empty());
    }
}
