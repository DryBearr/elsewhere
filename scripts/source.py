"""Builds the SideStore/AltStore source (source.json) from the repo's GitHub releases.

Usage:
    gh api --paginate repos/OWNER/REPO/releases --jq '.[]' \
        | python3 scripts/source.py https://owner.github.io/repo > site/source.json

Each line on stdin is one release object from the GitHub API. Releases without an
Elsewhere.ipa asset, drafts and pre-releases are skipped.
"""

import json
import sys

IPA_NAME = 'Elsewhere.ipa'
MIN_IOS = '15.0'
TINT = '#185E52'


def version_entry(release, ipa):
    version = release['tag_name'].removeprefix('v')
    notes = (release.get('body') or '').strip()
    return {
        'version': version,
        'buildVersion': version,  # the iOS build sets CFBundleVersion to the same value
        'date': release['published_at'],
        'localizedDescription': notes or f'Elsewhere {version}',
        'downloadURL': ipa['browser_download_url'],
        'size': ipa['size'],
        'minOSVersion': MIN_IOS,
    }


def build(pages_url, releases):
    versions = []
    for release in releases:
        if release.get('draft') or release.get('prerelease'):
            continue
        ipa = next((asset for asset in release.get('assets', []) if asset['name'] == IPA_NAME), None)
        if ipa:
            versions.append(version_entry(release, ipa))
    versions.sort(key=lambda entry: entry['date'], reverse=True)

    app = {
        'name': 'Elsewhere',
        'bundleIdentifier': 'io.github.drybearr.elsewhere',
        'developerName': 'DryBearr',
        'subtitle': 'Your media, elsewhere.',
        'localizedDescription': 'Elsewhere is a small, fully offline app. Everything stays on your device.',
        'iconURL': f'{pages_url}/icon.png',
        'tintColor': TINT,
        'category': 'utilities',
        'screenshots': [],
        'appPermissions': {'entitlements': [], 'privacy': {}},
        'versions': versions,
    }
    return {
        'name': 'Elsewhere',
        'identifier': 'io.github.drybearr.elsewhere.source',
        'subtitle': 'Builds of Elsewhere for SideStore and AltStore.',
        'iconURL': f'{pages_url}/icon.png',
        'website': pages_url,
        'sourceURL': f'{pages_url}/source.json',
        'tintColor': TINT,
        'nsfw': False,
        'apps': [app] if versions else [],
        'news': [],
    }


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    pages_url = sys.argv[1].rstrip('/')
    releases = [json.loads(line) for line in sys.stdin if line.strip()]
    json.dump(build(pages_url, releases), sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write('\n')


if __name__ == '__main__':
    main()
