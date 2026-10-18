CREATE TABLE IF NOT EXISTS object_usage (
    id INTEGER PRIMARY KEY CHECK(id=1),
    cached_bytes INTEGER NOT NULL CHECK(typeof(cached_bytes)='integer' AND cached_bytes>=0),
    protected_bytes INTEGER NOT NULL CHECK(typeof(protected_bytes)='integer' AND protected_bytes>=0)
);
INSERT INTO object_usage(id,cached_bytes,protected_bytes) VALUES(1,0,0);

CREATE TRIGGER IF NOT EXISTS objects_usage_insert AFTER INSERT ON objects BEGIN
    UPDATE object_usage SET
        cached_bytes=cached_bytes+CASE WHEN NEW.remote_confirmed=1 THEN NEW.size ELSE 0 END,
        protected_bytes=protected_bytes+CASE WHEN NEW.remote_confirmed=0 THEN NEW.size ELSE 0 END
    WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS objects_usage_delete AFTER DELETE ON objects BEGIN
    UPDATE object_usage SET
        cached_bytes=cached_bytes-CASE WHEN OLD.remote_confirmed=1 THEN OLD.size ELSE 0 END,
        protected_bytes=protected_bytes-CASE WHEN OLD.remote_confirmed=0 THEN OLD.size ELSE 0 END
    WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS objects_usage_update AFTER UPDATE OF size,remote_confirmed ON objects
WHEN OLD.size!=NEW.size OR OLD.remote_confirmed!=NEW.remote_confirmed BEGIN
    UPDATE object_usage SET
        cached_bytes=cached_bytes-CASE WHEN OLD.remote_confirmed=1 THEN OLD.size ELSE 0 END
            +CASE WHEN NEW.remote_confirmed=1 THEN NEW.size ELSE 0 END,
        protected_bytes=protected_bytes-CASE WHEN OLD.remote_confirmed=0 THEN OLD.size ELSE 0 END
            +CASE WHEN NEW.remote_confirmed=0 THEN NEW.size ELSE 0 END
    WHERE id=1;
END;
