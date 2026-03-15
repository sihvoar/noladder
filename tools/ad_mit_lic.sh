#!/bin/bash

AUTHOR="AP Sihvonen"
YEAR=$(date +%Y)

HEADER="// Copyright $YEAR $AUTHOR
// SPDX-License-Identifier: MIT
"

ROOT=${1:-.}
added=0
skipped=0

while IFS= read -r -d '' file; do
    if grep -q "SPDX-License-Identifier\|Copyright" "$file"; then
        echo "SKIP  $file"
        ((skipped++))
        continue
    fi

    tmp=$(mktemp)
    printf '%s\n' "$HEADER" | cat - "$file" > "$tmp" \
        && mv "$tmp" "$file"
    echo "ADD   $file"
    ((added++))

done < <(find "$ROOT" \
    -name "*.rs" \
    -not -path "*/target/*" \
    -print0)

echo ""
echo "$added added  $skipped skipped"
```

