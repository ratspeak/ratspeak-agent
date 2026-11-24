FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-astral python3-libzim curl ca-certificates libgomp1 \
    && useradd --system --create-home --home-dir /var/lib/ratspeak-pi ratspeak \
    && chmod 700 /var/lib/ratspeak-pi
WORKDIR /opt/ratspeak-pi
COPY release.tar.gz desktop-native.gz /tmp/
RUN tar xzf /tmp/release.tar.gz --strip-components=1 && \
    if [ "$(uname -m)" = x86_64 ]; then gzip -dc /tmp/desktop-native.gz > bin/ratspeak-pi-native; fi && \
    chmod 755 bin/ratspeak-pi-native && ldd bin/ratspeak-pi-native
ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1
USER ratspeak
ENTRYPOINT ["python3","-m","ratspeak_pi.desktop","serve"]
