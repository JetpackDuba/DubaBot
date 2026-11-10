FROM ubuntu:latest

RUN apt update && apt install -y curl gcc pkg-config libssl-dev cmake git python3 python3-pip libopus-dev
RUN mkdir -p ~/.local/bin/
RUN python3 -m pip install --break-system-packages -U yt-dlp

RUN curl https://sh.rustup.rs -sSf | bash -s -- -y
RUN ls ~/.cargo/bin
ENV PATH="/root/.cargo/bin:${PATH}"

RUN mkdir duba_bot
ADD . duba_bot/
RUN ls duba_bot/
WORKDIR duba_bot/
RUN mkdir output
RUN CARGO_TARGET_DIR=output cargo build --release
RUN ls output/release
RUN cp output/release/duba_bot ./
RUN chmod +x duba_bot
ENV PATH="$PATH:~/.local/bin/"
ENV PATH="$PATH:/duba_bot"
CMD duba_bot
