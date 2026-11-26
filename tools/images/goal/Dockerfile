# The image tests/container/goal.txt was taken in (R041): Python with FastAPI and
# uvicorn, and Debian's nginx: ADR 0011's goal in one container.
FROM python@sha256:e48f59cdba69a3cbd9ed2cf0f348d90d45a8d350d98134cf1070bc106ee9ed90
RUN apt-get update && apt-get install -y --no-install-recommends nginx && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir fastapi==0.165.0 uvicorn==0.54.0
CMD ["python3"]
