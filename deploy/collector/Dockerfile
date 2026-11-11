# Build context: a Linux culprit binary, LICENSE and THIRD-PARTY-LICENSES.txt.
# dev/smoke-container.py prepares that small context without sending local scans or credentials to Docker.
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -m 0700 -o 65532 -g 65532 /var/lib/culprit
COPY --chmod=0555 culprit /usr/local/bin/culprit
COPY --chmod=0444 LICENSE THIRD-PARTY-LICENSES.txt /usr/share/doc/culprit/
ENV CULPRIT_KEYCHAIN=off
WORKDIR /var/lib/culprit
USER 65532:65532
EXPOSE 9098
ENTRYPOINT ["/usr/local/bin/culprit"]
CMD ["collect", "service", "--help"]
