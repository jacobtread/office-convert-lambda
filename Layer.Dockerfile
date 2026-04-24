FROM public.ecr.aws/lambda/provided:al2023

# Install dependencies
RUN dnf -y update && \
    dnf -y install unzip && \
    dnf clean all

# Base function layer

WORKDIR /var/runtime

COPY ./layer/bootstrap.zip /tmp/bootstrap.zip

RUN unzip /tmp/bootstrap.zip && \
    rm /tmp/bootstrap.zip && \
    chmod +x /var/runtime/bootstrap

# Layer

COPY ./layer/layer.zip /tmp/layer.zip
RUN unzip /tmp/layer.zip -d / \
    && rm /tmp/layer.zip

ENV HOME=/tmp
ENV TMPDIR=/tmp

ENV LD_LIBRARY_PATH=/tmp/opt/libreoffice25.8/program:/tmp/opt/lib64:/usr/lib64

CMD [ "/var/runtime/bootstrap"]
