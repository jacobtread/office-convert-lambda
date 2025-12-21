FROM public.ecr.aws/lambda/provided:al2023

# Arguments
ARG PACKAGE_VERSION=25.8.3
ARG PACKAGE_BASE_URL="https://download.documentfoundation.org/libreoffice/stable"

# Install dependencies
RUN dnf -y update && \
    dnf -y install \
    nss \
    nspr \
    dbus-libs \
    cairo \
    libX11 \
    libXext \
    libX11-xcb \
    fontconfig \
    freetype \
    cups-libs \
    libxslt \
    wget \
    tar \
    gzip && \
    dnf clean all

# Download and install LibreOffice
RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "x86_64" ]; then ARCH="x86_64" ARCH_ALT="x86-64"; \
    elif [ "$ARCH" = "aarch64" ]; then ARCH="aarch64" ARCH_ALT="aarch64"; \
    else echo "Unsupported architecture: $ARCH" >&2; exit 1; fi && \
    PACKAGE_FILE="LibreOffice_${PACKAGE_VERSION}_Linux_${ARCH_ALT}_rpm.tar.gz" && \
    FOLDER_NAME="LibreOffice_${PACKAGE_VERSION}_Linux_${ARCH_ALT}_rpm" && \
    DOWNLOAD_URL="${PACKAGE_BASE_URL}/${PACKAGE_VERSION}/rpm/${ARCH}/${PACKAGE_FILE}"  && \
    wget -q -P /tmp ${DOWNLOAD_URL} && \
    tar -xzf /tmp/$PACKAGE_FILE && \
    cd LibreOffice_*_Linux_${ARCH_ALT}_rpm/RPMS && \
    rpm -Uvh *.rpm && \
    dnf clean all

ARG LAMBDA_VERSION=0.1.0

# Download lambda script
RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "x86_64" ]; then FILE="bootstrap"; \
    elif [ "$ARCH" = "aarch64" ]; then FILE="bootstrap-arm64"; \
    else echo "Unsupported architecture: $ARCH" >&2; exit 1; fi && \
    curl -L -o /var/runtime/bootstrap https://github.com/jacobtread/office-convert-lambda/releases/download/${LAMBDA_VERSION}/${FILE} && \
    chmod +x /var/runtime/bootstrap

# Update library path to ensure the libreoffice versions are used
ENV LO_PATH=/opt/libreoffice25.8/program
ENV LD_LIBRARY_PATH=${LO_PATH}

# https://wiki.documentfoundation.org/Development/Environment_variables
ENV SAL_USE_VCLPLUGIN=gen
ENV SAL_DISABLE_USERMIGRATION=true
ENV SAL_DISABLE_LOCKING=1

# Forcefully override the paths libreoffice uses
# (Lambda file system is mostly readonly so these need to use paths from /tmp)
RUN mkdir /tmp/lo_home && mkdir /tmp/lo_profile
ENV UserInstallation=file:///tmp/lo_profile
ENV URE_BOOTSTRAP=file:///opt/libreoffice25.8/program/fundamentalrc

CMD [ "/var/runtime/bootstrap"]
