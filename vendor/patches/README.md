# Decoder patches

These patch series are applied by `vendor/build-decoders.sh` on top of the
upstream commits pinned there.

They modify FFmpeg and libde265 and are therefore derivative works under
the upstream licenses — FFmpeg patches: LGPL-2.1-or-later; libde265
patches: LGPL-3.0-or-later. They are **not** covered by streamlens's
MIT/Apache-2.0 license.

To change decoder code: apply the series onto the pinned upstream commit,
develop and commit there, then regenerate with
`git format-patch <pinned-commit>..HEAD` into this directory. The repo
must stay buildable from these patches alone — CI builds them from
scratch.
