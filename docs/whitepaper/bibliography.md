# Bibliography for the bulkload whitepaper

Companion to [bulkload.md](bulkload.md) (OI-1003-Q7). There is one entry per
reference, keyed as the paper cites it. Every entry was checked on
2026-10-03 against a DOI record, an ISBN, or the stable official page or
repository named in it. "Verified via" names the tool and the identifier
checked:

- **Crossref** means the DOI resolved through the Crossref metadata service,
  and the authors, title, venue, year and pages below are Crossref's.
- **Fetched** means the URL was retrieved and its title and authorship
  matched.
- **Search** means a web search located the record, which was then fetched
  where possible.

Software documentation has no fixed publication year. It is cited as
"accessed 2026-10-03". Nothing here is quoted at length; the paper
paraphrases.

## Papers, reports and books

**[ALICE14]** Thanumalayan Sankaranarayana Pillai, Vijay Chidambaram,
Ramnatthan Alagappan, Samer Al-Kiswany, Andrea C. Arpaci-Dusseau and
Remzi H. Arpaci-Dusseau. "All File Systems Are Not Created Equal: On the
Complexity of Crafting Crash-Consistent Applications." *11th USENIX
Symposium on Operating Systems Design and Implementation (OSDI 14)*, USENIX
Association, 2014, pp. 433–448. ISBN 978-1-931971-16-4.
<https://www.usenix.org/conference/osdi14/technical-sessions/presentation/pillai>
- Verified via: fetched the USENIX presentation page (title, authors,
  venue, ISBN, pages). The method is also named in
  `crates/bulkload-agent/src/io/crash_check.rs`.
- The paper's description of ALICE's method comes from the paper itself,
  downloaded from USENIX
  (<https://www.usenix.org/system/files/conference/osdi14/osdi14-paper-pillai.pdf>)
  and read as text: abstract persistence models (section 3.2); the crash
  states it constructs (prefixes, single-call intermediate states, pairwise
  ordering) in section 3.3; and the limitations in section 3.6.

**[B3-18]** Jayashree Mohan, Ashlie Martinez, Soujanya Ponnapalli, Pandian
Raju and Vijay Chidambaram. "Finding Crash-Consistency Bugs with Bounded
Black-Box Crash Testing." *13th USENIX Symposium on Operating Systems Design
and Implementation (OSDI 18)*, USENIX Association, 2018, pp. 33–50. ISBN
978-1-939133-08-3.
<https://www.usenix.org/conference/osdi18/presentation/mohan>
- Verified via: fetched the USENIX presentation page. Title, authors, venue,
  ISBN and pages come from the BibTeX record embedded in it
  (`pages = {33--50}`).

**[CrashMonkey19]** Jayashree Mohan, Ashlie Martinez, Soujanya Ponnapalli,
Pandian Raju and Vijay Chidambaram. "CrashMonkey and ACE: Systematically
Testing File-System Crash Consistency." *ACM Transactions on Storage*
15(2), 2019, pp. 1–34. DOI
[10.1145/3320275](https://doi.org/10.1145/3320275).
- Verified via: Crossref API, DOI 10.1145/3320275 (authors, title,
  subtitle, journal, volume, issue, pages, 2019).

**[FastCDC16]** Wen Xia, Yukun Zhou, Hong Jiang, Dan Feng, Yu Hua, Yuchong
Hu, Yucheng Zhang and Qing Liu. "FastCDC: A Fast and Efficient
Content-Defined Chunking Approach for Data Deduplication." *2016 USENIX
Annual Technical Conference (USENIX ATC 16)*, USENIX Association, 2016.
ISBN 978-1-931971-30-0.
<https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia>
- Verified via: fetched the USENIX presentation page (title, authors,
  venue, ISBN).

**[FastCDC20]** Wen Xia, Xiangyu Zou, Hong Jiang, Yukun Zhou, Chuanyi Liu,
Dan Feng, Yu Hua, Yuchong Hu and Yucheng Zhang. "The Design of Fast
Content-Defined Chunking for Data Deduplication Based Storage Systems."
*IEEE Transactions on Parallel and Distributed Systems* 31(9), 2020,
pp. 2017–2031. DOI
[10.1109/TPDS.2020.2984632](https://doi.org/10.1109/TPDS.2020.2984632).
- Verified via: Crossref, DOI 10.1109/TPDS.2020.2984632 (title, volume 31,
  issue 9, pages 2017–2031). Its Crossref primary resource is IEEE Xplore
  document 9055082.
- Note: bulkload uses the `fastcdc` crate's `v2020` module
  (`crates/bulkload-agent/src/hash.rs`), at version 3.2.1 in `Cargo.lock`.
  Re-resolved on 2026-10-03: that version's `src/v2020/mod.rs`, read from
  the crates.io package, names this paper as IEEE Xplore document 9055082,
  the same record as the DOI. Whether the code matches the paper is the
  crate's claim; it was not checked here.

**[FileSync98]** S. Balasubramaniam and Benjamin C. Pierce. "What is a file
synchronizer?" *Proceedings of the 4th Annual ACM/IEEE International
Conference on Mobile Computing and Networking (MobiCom '98)*, ACM, 1998,
pp. 98–108. DOI
[10.1145/288235.288261](https://doi.org/10.1145/288235.288261).
- Verified via: Crossref, DOI 10.1145/288235.288261. Also listed on Pierce's
  Unison bibliography page,
  <https://www.cis.upenn.edu/~bcpierce/papers/unison_bib.html> (fetched).

**[LBFS01]** Athicha Muthitacharoen, Benjie Chen and David Mazières. "A
Low-bandwidth Network File System." *Proceedings of the Eighteenth ACM
Symposium on Operating Systems Principles (SOSP '01)*, ACM, 2001,
pp. 174–187. DOI
[10.1145/502034.502052](https://doi.org/10.1145/502034.502052).
- Verified via: Crossref, DOI 10.1145/502034.502052.

**[OptFS13]** Vijay Chidambaram, Thanumalayan Sankaranarayana Pillai,
Andrea C. Arpaci-Dusseau and Remzi H. Arpaci-Dusseau. "Optimistic Crash
Consistency." *Proceedings of the Twenty-Fourth ACM Symposium on Operating
Systems Principles (SOSP '13)*, ACM, 2013, pp. 228–243. DOI
[10.1145/2517349.2522726](https://doi.org/10.1145/2517349.2522726).
- Verified via: Crossref, DOI 10.1145/2517349.2522726.

**[QuickCheck00]** Koen Claessen and John Hughes. "QuickCheck: a lightweight
tool for random testing of Haskell programs." *Proceedings of the Fifth ACM
SIGPLAN International Conference on Functional Programming (ICFP '00)*,
ACM, 2000, pp. 268–279. DOI
[10.1145/351240.351266](https://doi.org/10.1145/351240.351266).
- Verified via: Crossref, DOI 10.1145/351240.351266. The title and subtitle
  come from the Crossref API record.

**[Rsync96]** Andrew Tridgell and Paul Mackerras. "The rsync algorithm."
Technical Report TR-CS-96-05, Department of Computer Science, Australian
National University, 1996. Handle
<http://hdl.handle.net/1885/40765>. HTML edition:
<https://rsync.samba.org/tech_report/>.
- Verified via: fetched the ANU digital collections record (handle
  1885/40765: title, authors, 1996, report number). Also fetched the rsync
  project's HTML copy (title, authors, ANU affiliation).

**[Specifying02]** Leslie Lamport. *Specifying Systems: The TLA+ Language and
Tools for Hardware and Software Engineers.* Addison-Wesley, 2002. ISBN
978-0-321-14306-8.
- Verified via: fetched Lamport's book page,
  <https://lamport.azurewebsites.net/tla/book.html> (title, author,
  publisher, year, ISBN 9780321143068).

**[TLA94]** Leslie Lamport. "The temporal logic of actions." *ACM
Transactions on Programming Languages and Systems* 16(3), 1994,
pp. 872–923. DOI
[10.1145/177492.177726](https://doi.org/10.1145/177492.177726).
- Verified via: Crossref, DOI 10.1145/177492.177726.

**[TLC99]** Yuan Yu, Panagiotis Manolios and Leslie Lamport. "Model Checking
TLA+ Specifications." In Laurence Pierre and Thomas Kropf (eds.), *Correct
Hardware Design and Verification Methods (CHARME '99)*, Lecture Notes in
Computer Science 1703, Springer, 1999, pp. 54–66. DOI
[10.1007/3-540-48153-2_6](https://doi.org/10.1007/3-540-48153-2_6).
- Verified via: Crossref API, DOI 10.1007/3-540-48153-2_6 (authors, title,
  container titles, year, pages, ISBN 9783540665595). The editors come
  from Crossref's record for the book, DOI 10.1007/3-540-48153-2.
- LNCS volume number: re-resolved on 2026-10-03. Crossref, OpenAlex and the
  Springer chapter page gave no volume number. Springer's page served a bot
  check, and DBLP returned an error. Lamport's publications page,
  <https://lamport.azurewebsites.net/pubs/pubs.html> (fetched), lists the
  paper in CHARME '99 as LNCS number 1703, edited by Pierre and Kropf,
  pp. 54–66.

**[Unison04]** Benjamin C. Pierce and Jérôme Vouillon. "What's in Unison? A
Formal Specification and Reference Implementation of a File Synchronizer."
Technical Report MS-CIS-03-36, Department of Computer and Information
Science, University of Pennsylvania, 24 February 2004.
<https://repository.upenn.edu/handle/20.500.14332/7325>
- Verified via: the repository API record. The handle URL was fetched
  (HTTP 200), and the item's record was read from the repository's REST API
  (`/server/api/core/items/df9bb5bd-2f08-4333-89ee-198156a9cf1f`): title,
  authors, report number MS-CIS-03-36, issue date 2004-02-24, and this
  handle as its URI. Also fetched Pierce's Unison bibliography page,
  <https://www.cis.upenn.edu/~bcpierce/papers/unison_bib.html> (authors,
  title, institution, report number, 2004).

## Specifications and official documentation

**[AppleDiskWrites]** Apple Inc. "Reducing disk writes." Apple Developer
Documentation (Xcode). Accessed 2026-10-03.
<https://developer.apple.com/documentation/xcode/reducing-disk-writes>
- Verified via: fetched the page and its JSON data endpoint,
  <https://developer.apple.com/tutorials/data/documentation/xcode/reducing-disk-writes.json>,
  and read the body. Re-resolved on 2026-10-03: the JSON endpoint was
  fetched again (HTTP 200, title "Reducing disk writes"), and its body
  still says:
  - apps that need a write barrier can use `F_BARRIERFSYNC`;
  - `F_FULLFSYNC` is for apps that need a strong expectation of
    persistence;
  - `F_FULLFSYNC` is best effort, and data can still be lost on sudden
    power loss.
- The page says nothing about drive-wide flushing. The paper cites it only
  for that guidance; the drain and barrier semantics come from
  [AppleFcntl].

**[AppleFcntl]** Apple Inc. "fcntl(2)" manual page, in the XNU kernel
source as `bsd/man/man2/fcntl.2` (page date 12 August 2021). Accessed
2026-10-03.
<https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/fcntl.2>
Older copy, which has no `F_BARRIERFSYNC` entry:
<https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fcntl.2.html>.
- Verified via: fetched the raw man source from the
  `apple-oss-distributions/xnu` repository and read both entries:
  - `F_FULLFSYNC` asks the drive to flush all buffered data, draining the
    device's whole queue;
  - `F_BARRIERFSYNC` issues a barrier that orders earlier flushed I/O on
    the same device ahead of later I/O, with no durability on return, and
    needs hardware support that Apple SSDs are guaranteed to provide.
- Also fetched the archive page ("Mac OS X Manual Page For fcntl(2)"),
  whose `F_FULLFSYNC` entry matches.
- `crates/bulkload-agent/src/io/crash_check.rs` quotes this manual page.

**[BLAKE3]** Jack O'Connor, Jean-Philippe Aumasson, Samuel Neves and Zooko
Wilcox-O'Hearn. "BLAKE3: one function, fast everywhere." Specification,
version 20211102173700 (2 November 2021).
<https://github.com/BLAKE3-team/BLAKE3-specs/blob/master/blake3.pdf>
Reference implementation: <https://github.com/BLAKE3-team/BLAKE3>.
- Verified via: downloaded the PDF from the specification repository and
  extracted its text. The title, authors and version string come from page
  1. The 1024-byte chunks and the three modes (hash, keyed_hash,
  derive_key) come from section 2.
- Also fetched the BLAKE3 repository README (designers, modes).

**[GitBundle]** The Git project. "git-bundle: Move objects and refs by
archive." Git documentation. Accessed 2026-10-03.
<https://git-scm.com/docs/git-bundle>
- Verified via: fetched (page title and NAME line; prerequisites; incremental
  bundles from revision ranges).

**[GitPackObjects]** The Git project. "git-pack-objects: Create a packed
archive of objects." Git documentation. Accessed 2026-10-03.
<https://git-scm.com/docs/git-pack-objects>
- Verified via: fetched (page title and NAME line; `--thin`, which needs
  `index-pack --fix-thin` on receipt; `--delta-base-offset`).

**[GitPackProto]** The Git project. "gitprotocol-pack: How packs are
transferred over-the-wire." Git documentation. Accessed 2026-10-03.
<https://git-scm.com/docs/gitprotocol-pack>
- Verified via: fetched (page title and NAME line; want and have
  negotiation; the `thin-pack` capability).

**[IoprioSet]** The Linux man-pages project. "ioprio_get(2),
ioprio_set(2): get/set I/O scheduling class and priority." Accessed
2026-10-03. <https://man7.org/linux/man-pages/man2/ioprio_set.2.html>
- Verified via: fetched (page title and NAME line; the idle class gets disk
  time only when no other process needs it; classes take effect only under
  a scheduler that implements them).

**[RacyGit]** The Git project. "racy-git." Git technical documentation.
Accessed 2026-10-03.
<https://git-scm.com/docs/racy-git>
- Verified via: fetched, title "Git - racy-git Documentation". Re-resolved
  on 2026-10-03 by fetching it again and reading the body: an entry is
  racily clean when its cached `st_mtime` is the same as, or newer than,
  the index file's own timestamp. Git then re-checks the entry's content
  against the recorded object.

**[SQLiteBackup]** SQLite. "SQLite Backup API." Accessed 2026-10-03.
<https://www.sqlite.org/backup.html>
- Verified via: fetched (incremental `sqlite3_backup_step`; the read lock is
  held only while a step reads; the backup restarts on a write from another
  connection).

**[SQLiteWAL]** SQLite. "Write-Ahead Logging." Accessed 2026-10-03.
<https://www.sqlite.org/wal.html>
- Verified via: fetched (separating a database from its WAL can lose
  committed transactions; the role of `-shm`; the conditions for read-only
  WAL access).

**[ZfsSend]** OpenZFS. "zfs-send(8)." OpenZFS documentation, manual pages.
Accessed 2026-10-03.
<https://openzfs.github.io/openzfs-docs/man/master/8/zfs-send.8.html>
- Verified via: fetched (incremental `-i`/`-I` streams between snapshots;
  resumable receive with `receive_resume_token` and `-t`).

## Software projects

**[Borg]** The BorgBackup project. Borg documentation, version 1.4.5 at
access: "Data structures and file formats" (Internals) and "borg create"
(Usage). Source: `src/borg/cache.py` on the `1.4-maint` branch. Accessed
2026-10-03.
- <https://borgbackup.readthedocs.io/en/stable/internals/data-structures.html>
- <https://borgbackup.readthedocs.io/en/stable/usage/create.html>
- <https://github.com/borgbackup/borg/blob/1.4-maint/src/borg/cache.py>
- Verified via: fetched all three:
  - data structures: the buzhash chunker; chunk ids by cryptographic hash
    or MAC; the files cache used to skip unchanged files;
  - `borg create`: `--files-cache` defaults to `ctime,size,inode`;
  - `cache.py`: `LocalCache.commit` does not persist entries whose cmtime
    is the newest seen (lines 629–647 at access).

**[Bup]** The bup project. "The Crazy Hacker's Crazy Guide to Bup
Craziness" (`DESIGN.md`). Accessed 2026-10-03.
<https://github.com/bup/bup/blob/main/DESIGN.md>
- Verified via: fetched the raw file. The title is its first line, checked
  word for word. Also read: Git packfile storage, rolling-checksum
  hashsplitting, the separate bupindex, and midx.

**[Casync17]** Lennart Poettering. "casync — A tool for distributing file
system images." Blog post, 20 June 2017.
<http://0pointer.net/blog/casync-a-tool-for-distributing-file-system-images.html>
Repository: <https://github.com/systemd/casync>.
- Verified via: fetched the post (title, author, date; buzhash chunking;
  `.caidx`/`.caibx`/`.catar`). Fetched the repository page (owner `systemd`,
  not archived) and its README.
- Digest: the README names SHA512/256 as the chunk digest, with SHA256 as
  the alternative. The 2017 post names SHA256 only. The paper describes
  casync in the present tense, so it follows the README.

**[Desync]** folbricht. "desync: Alternative casync implementation."
Software repository. Accessed 2026-10-03.
<https://github.com/folbricht/desync>
- Verified via: fetched (casync-compatible formats and chunk stores,
  parallel chunking, BSD-3-Clause).

**[Proptest]** The proptest authors. "proptest: Hypothesis-like property
testing for Rust." Software repository. Accessed 2026-10-03.
<https://github.com/proptest-rs/proptest>
- Verified via: fetched, plus the GitHub API repository description
  (strategies, shrinking, Apache-2.0 or MIT).
- bulkload declares `proptest` version `1` as a workspace dependency, and
  `Cargo.lock` resolves it to 1.11.0 (checked at `6268175`;
  [property-test plan](../plans/2026-10-03-property-test-plan.md),
  section 0).

**[Rclone]** The rclone project. rclone documentation: "rclone copy", "Local
Filesystem" and "Frequently Asked Questions." Accessed 2026-10-03.
- <https://rclone.org/commands/rclone_copy/>
- <https://rclone.org/local/>
- <https://rclone.org/faq/>
- Verified via: fetched all three:
  - `copy` skips files by size and modification time, or by checksum;
  - local-to-local copies clone on APFS unless `--local-no-clone` is given;
  - the FAQ entry on why rclone does not do partial transfers or binary
    diffs.
- The version measured in bulkload's evidence is rclone v1.75.0
  ([r23-2026-09-18](../evidence/r23-2026-09-18.md)).

**[Restic]** The restic authors. "References" (the repository design
chapter). restic documentation, version 0.19.1 at access. Accessed
2026-10-03.
<https://restic.readthedocs.io/en/stable/100_references.html>
- Verified via: fetched (Rabin-fingerprint chunking of 512 KiB to 8 MiB;
  SHA-256 ids; encrypted packs, index and snapshots).

**[RsyncMan]** The rsync project. "rsync(1)" manual page. Accessed
2026-10-03. <https://download.samba.org/pub/rsync/rsync.1>
Project home: <https://rsync.samba.org/>.
- Verified via: fetched the manual page and read:
  - the default quick check by size and modification time;
  - `--checksum`: the sender checksums every file during its file-list
    scan, and the receiver checksums every file whose size matches the
    sender's;
  - `--partial`, `--inplace` and `--append`.

  Fetched the project home page.

**[UnisonRepo]** Benjamin C. Pierce and contributors. "unison: Unison file
synchronizer." Software repository. Accessed 2026-10-03.
<https://github.com/bcpierce00/unison>
- Verified via: fetched, plus the GitHub API repository description
  (bidirectional synchronizer, archive-based update detection, conflicts
  surfaced; GPL-3.0).
