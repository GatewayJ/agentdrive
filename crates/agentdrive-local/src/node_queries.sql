CREATE INDEX IF NOT EXISTS nodes_parent ON nodes(branch, parent, path);
CREATE TABLE IF NOT EXISTS node_usage (
    branch TEXT PRIMARY KEY,
    inodes INTEGER NOT NULL CHECK(inodes >= 0),
    bytes INTEGER NOT NULL CHECK(typeof(bytes) = 'integer' AND bytes >= 0)
);
CREATE TRIGGER IF NOT EXISTS nodes_usage_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO node_usage(branch,inodes,bytes)
    VALUES(NEW.branch,1,json_extract(CAST(NEW.data AS TEXT),'$.inode.size'))
    ON CONFLICT(branch) DO UPDATE SET inodes=inodes+1,bytes=bytes+excluded.bytes;
END;
CREATE TRIGGER IF NOT EXISTS nodes_usage_delete AFTER DELETE ON nodes BEGIN
    UPDATE node_usage SET inodes=inodes-1,
        bytes=bytes-json_extract(CAST(OLD.data AS TEXT),'$.inode.size') WHERE branch=OLD.branch;
END;
CREATE TRIGGER IF NOT EXISTS nodes_usage_update AFTER UPDATE OF data ON nodes BEGIN
    UPDATE node_usage SET bytes=bytes-json_extract(CAST(OLD.data AS TEXT),'$.inode.size')
        +json_extract(CAST(NEW.data AS TEXT),'$.inode.size') WHERE branch=NEW.branch;
END;
