#!/bin/bash

# Script to sync documentation from private pod repo to public apt-package repo
# Usage: ./scripts/sync-docs.sh [version]
# Public repo: Xandeum/pod-apt-package

set -e

VERSION="${1:-$(grep '^version = ' Cargo.toml | cut -d'"' -f2)}"
APT_REPO_PATH="../pod-apt-package"  # Default path for Xandeum/pod-apt-package

echo "🔄 Syncing documentation to public apt package repo..."
echo "📁 Target repo: $APT_REPO_PATH (Xandeum/pod-apt-package)"
echo "🏷️  Version: $VERSION"

# Check if apt repo exists
if [ ! -d "$APT_REPO_PATH" ]; then
    echo "❌ Error: Public apt package repo not found at $APT_REPO_PATH"
    echo "💡 Please clone it first:"
    echo "   git clone https://github.com/Xandeum/pod-apt-package.git ../pod-apt-package"
    exit 1
fi

# Create docs directory in apt repo if it doesn't exist
mkdir -p "$APT_REPO_PATH/docs"

# Copy documentation files
echo "📋 Copying documentation files..."
cp -r docs/* "$APT_REPO_PATH/docs/"
cp mkdocs.yml "$APT_REPO_PATH/"
cp -r .github "$APT_REPO_PATH/" 2>/dev/null || true

# Update version in the copied files
echo "🔄 Updating version references to $VERSION..."
cd "$APT_REPO_PATH"

# Update mkdocs.yml
sed -i "s/site_name: Xandeum Pod Documentation.*/site_name: Xandeum Pod Documentation v$VERSION/" mkdocs.yml

# Update version references in docs
find docs/ -name "*.md" -exec sed -i "s/pod [0-9]\+\.[0-9]\+\.[0-9]\+/pod $VERSION/g" {} \;
find docs/ -name "*.md" -exec sed -i "s/Pod v[0-9]\+\.[0-9]\+\.[0-9]\+/Pod v$VERSION/g" {} \;

# Add version badge to homepage if it's a release
if [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    if ! grep -q "!!! info \"Version\"" docs/index.md; then
        sed -i '/^# Xandeum Pod Documentation/a\\n!!! info "Version"\n    This documentation is for **Xandeum Pod v'$VERSION'**\n    Released: '$(date +"%B %d, %Y") docs/index.md
    else
        # Update existing version info
        sed -i "s/Xandeum Pod v[0-9]\+\.[0-9]\+\.[0-9]\+/Xandeum Pod v$VERSION/g" docs/index.md
        sed -i "s/Released: .*/Released: $(date +"%B %d, %Y")/g" docs/index.md
    fi
fi

echo "✅ Documentation synced successfully!"
echo "📝 Next steps:"
echo "   1. cd $APT_REPO_PATH"
echo "   2. git add ."
echo "   3. git commit -m \"Update documentation for v$VERSION\""
echo "   4. git tag v$VERSION"
echo "   5. git push origin main --tags"
echo ""
echo "🌐 Documentation will be available at: https://xandeum.github.io/pod-apt-package/" 