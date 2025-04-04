## 0.8.0-alpha - unreleased

- Raise MSRV to 1.65.
  See [PR 3715].

- Add hole punching support by implementing `Transport::dial_as_listener`. See [PR 3964].

[PR 3715]: https://github.com/libp2p/rust-libp2p/pull/3715
[PR 3964]: https://github.com/libp2p/rust-libp2p/pull/3964

## 0.7.0-alpha.3

- Depend `libp2p-tls` `v0.1.0`.

## 0.7.0-alpha.2

- Update to `libp2p-tls` `v0.1.0-alpha.2`.

- Update to `libp2p-core` `v0.39.0`.

- Add opt-in support for the `/quic` codepoint, interpreted as QUIC version draft-29.
  See [PR 3151].

- Wake the transport's task when a new dialer or listener is added. See [3342].

- Discard correct waker upon accepting inbound stream. See [PR 3420].

[PR 3151]: https://github.com/libp2p/rust-libp2p/pull/3151
[PR 3342]: https://github.com/libp2p/rust-libp2p/pull/3342
[PR 3420]: https://github.com/libp2p/rust-libp2p/pull/3420

## 0.7.0-alpha

- Initial alpha release.
