# Private Node package support

Build-time input for components that already use Node. Each product owns and
packages its own executable; this creates no global Node, npm dependency,
background service or shared installation requirement.

Vendor this directory from an exact umbrella revision, recording the revision
and file hashes. `node_support.prepare(target, cache, new_directory)` obtains
the checksummed official distribution and emits only `node` (or `node.exe`) and
its complete `LICENSE`. The six target names match the lock. The helper refuses
used destinations, ambiguous members, links and mismatched archives.

The lock consolidates Companion's Node 22.23.5 native input set. It does not
package npm or an application's dependencies: components must retain their
lockfile provenance and notices, bundle production dependencies, and qualify
imports with the exact native Node. No package manager runs at user startup.

Run `python packaging/node/test_support.py`. CI also runs the actual pinned
executable on all six targets. Product setup, callbacks, authentication and
publisher signing remain component responsibilities; a Node check alone does
not establish the complete installation experience.

Primary source: [official Node distribution and checksums](https://nodejs.org/dist/v22.23.5/).
