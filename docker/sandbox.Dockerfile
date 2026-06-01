FROM python:3.12-slim

# Languages + coreutils (provides `timeout`, `sleep`).
RUN apt-get update && apt-get install -y --no-install-recommends \
        nodejs bash coreutils ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Common Python libs (trim/extend to taste).
RUN pip install --no-cache-dir numpy pandas requests

# Non-root user 1000 matches `docker run -u 1000:1000`.
RUN useradd -m -u 1000 sandbox
USER 1000:1000
WORKDIR /home/sandbox
# 容器命令由 gateway 传入 (`sleep infinity`)，保持常驻，靠 docker exec 进入。