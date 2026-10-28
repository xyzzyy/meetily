"""Downloads the pinned Nemotron export. Meetily checks each file's SHA-256 afterwards."""
import sys

from huggingface_hub import snapshot_download


def main():
    repo, revision, local_dir, *files = sys.argv[1:]
    # The files are public. A token in the user's environment is never sent.
    snapshot_download(repo, revision=revision, local_dir=local_dir, allow_patterns=files, token=False)


if __name__ == "__main__":
    main()
