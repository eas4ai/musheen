FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        apache2 \
        apache2-utils \
        ca-certificates \
        netcat-openbsd \
        openssh-server \
        openssl \
        vsftpd \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --home-dir /srv/remote --shell /bin/bash musheen \
    && printf '%s\n' 'musheen:musheen-pass' | chpasswd \
    && usermod --append --groups www-data musheen \
    && usermod --append --groups musheen www-data \
    && install --directory --owner=musheen --group=musheen --mode=2770 /srv/remote/fixtures /srv/remote/paged \
    && printf '%s\n' 'remote fixture payload' > /srv/remote/fixtures/range.txt \
    && seq -w 0000 0519 | while read -r index; do : > "/srv/remote/paged/item-$index.txt"; done \
    && chown musheen:musheen /srv/remote/fixtures/range.txt \
    && chown -R musheen:musheen /srv/remote/paged \
    && chmod 0660 /srv/remote/fixtures/range.txt \
    && install --directory --mode=0755 /run/sshd /run/vsftpd/empty \
    && install --directory --owner=www-data --group=www-data --mode=0750 /var/lock/apache2 \
    && htpasswd -bc /etc/apache2/remote-ci.passwd musheen musheen-pass \
    && openssl req -x509 -newkey rsa:2048 -nodes -days 7 \
        -subj '/CN=Musheen Remote CI CA' \
        -addext 'basicConstraints=critical,CA:TRUE' \
        -addext 'keyUsage=critical,keyCertSign,cRLSign' \
        -keyout /etc/ssl/private/musheen-remote-ca.key \
        -out /etc/ssl/certs/musheen-remote-ca.crt \
    && openssl req -new -newkey rsa:2048 -nodes \
        -subj '/CN=localhost' \
        -keyout /etc/ssl/private/musheen-remote-ci.key \
        -out /tmp/musheen-remote-ci.csr \
    && printf '%s\n' \
        'subjectAltName=DNS:localhost,IP:127.0.0.1' \
        'basicConstraints=critical,CA:FALSE' \
        'keyUsage=critical,digitalSignature,keyEncipherment' \
        'extendedKeyUsage=serverAuth' \
        > /tmp/musheen-remote-ci.ext \
    && openssl x509 -req -days 7 \
        -in /tmp/musheen-remote-ci.csr \
        -CA /etc/ssl/certs/musheen-remote-ca.crt \
        -CAkey /etc/ssl/private/musheen-remote-ca.key \
        -CAcreateserial \
        -extfile /tmp/musheen-remote-ci.ext \
        -out /etc/ssl/certs/musheen-remote-ci.crt \
    && rm -f /tmp/musheen-remote-ci.csr /tmp/musheen-remote-ci.ext \
    && rm -f /etc/ssh/ssh_host_ed25519_key /etc/ssh/ssh_host_ed25519_key.pub \
    && ssh-keygen -q -t ed25519 -N '' -f /etc/ssh/ssh_host_ed25519_key \
    && a2enmod auth_basic dav dav_fs ssl \
    && a2dissite 000-default default-ssl

COPY ci/remote-services/apache.conf /etc/apache2/sites-available/remote-ci.conf
COPY ci/remote-services/sshd_config /etc/ssh/sshd_config
COPY ci/remote-services/vsftpd-ftp.conf /etc/vsftpd-ftp.conf
COPY ci/remote-services/vsftpd-ftps.conf /etc/vsftpd-ftps.conf
COPY ci/remote-services/entrypoint.sh /usr/local/bin/musheen-remote-services

RUN a2ensite remote-ci \
    && chmod 0755 /usr/local/bin/musheen-remote-services

# FTP, implicit FTPS, SFTP, HTTP, HTTPS/WebDAV, and passive FTP data ranges.
EXPOSE 2121 2990 2222 8080 8443 30000-30009 30100-30109

HEALTHCHECK --interval=1s --timeout=1s --start-period=3s --retries=30 \
    CMD test -f /run/musheen-remote-ready \
        && nc -z 127.0.0.1 2121 \
        && nc -z 127.0.0.1 2990 \
        && nc -z 127.0.0.1 2222 \
        && nc -z 127.0.0.1 8080 \
        && nc -z 127.0.0.1 8443

ENTRYPOINT ["/usr/local/bin/musheen-remote-services"]
