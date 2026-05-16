rsync -av --delete \
  --exclude='target/' \
  --exclude='.git/' \
  --exclude='.idea/' \
  --exclude='node_modules/' \
  --exclude='.env' \
  --exclude='.DS_Store' \
  --exclude='*.log' \
  --exclude='*HANDOFF*.md' \
  --exclude='*PATCH*.md' \
  --exclude='*verify*.sh' \
  --exclude='stockpile.duckdb' \
  --exclude='situation_room.duckdb' \
  --exclude='*.duckdb' \
  --exclude='*.duckdb.wal' \
  ~/Documents/Claude/Projects/SituationRoom/ \
  /Users/aben/RustRoverProjects/situation_room/