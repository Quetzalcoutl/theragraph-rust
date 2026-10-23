-- TheraFlix became TheraVid (founder, 2026-10-23). The code says `vid` everywhere; this carries what was STORED over.
--
--   * a content/contract type column: the value 'flix' is now 'vid'
--   * user_preferences.flix_affinity -> vid_affinity (the affinity the engine keeps per content type)
--   * the decay_old_preferences function, when it exists (the indexer's migration creates it): its body names the column
--
-- Idempotent and guarded: a table or column that is not there is skipped, and the second run changes nothing. The theragraph-indexer's
-- migration 20261023100000 carries the same step (and more: its own tables), because either service may set the shared database up first.
-- The migrations that created the old names (002, 015) are history and are NOT edited: sqlx checks their checksums.

DO $$
DECLARE
    t record;
BEGIN
    FOR t IN SELECT * FROM (VALUES
        ('user_interactions', 'nft_contract_type'),
        ('trending_snapshots', 'contract_type'),
        ('pending_interactions', 'contract_type'),
        ('failed_logs_dlq', 'contract_type'),
        ('indexer_state', 'contract_type')
    ) AS v(tbl, col)
    LOOP
        IF to_regclass('public.' || t.tbl) IS NOT NULL
           AND EXISTS (SELECT 1 FROM information_schema.columns
                       WHERE table_schema = 'public' AND table_name = t.tbl AND column_name = t.col) THEN
            EXECUTE format('UPDATE %I SET %I = %L WHERE %I = %L', t.tbl, t.col, 'vid', t.col, 'flix');
        END IF;
    END LOOP;

    IF to_regclass('public.user_preferences') IS NOT NULL
       AND EXISTS (SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'public' AND table_name = 'user_preferences' AND column_name = 'flix_affinity')
       AND NOT EXISTS (SELECT 1 FROM information_schema.columns
                       WHERE table_schema = 'public' AND table_name = 'user_preferences' AND column_name = 'vid_affinity') THEN
        ALTER TABLE user_preferences RENAME COLUMN flix_affinity TO vid_affinity;
    END IF;

    IF to_regprocedure('decay_old_preferences(double precision)') IS NOT NULL THEN
        EXECUTE $fn$
            CREATE OR REPLACE FUNCTION decay_old_preferences(decay_factor FLOAT DEFAULT 0.95)
            RETURNS void AS $body$
            DECLARE
              one_day_ago TIMESTAMP := NOW() - INTERVAL '1 day';
            BEGIN
              UPDATE user_preferences
              SET
                snap_affinity = GREATEST(0.5, snap_affinity * decay_factor),
                art_affinity = GREATEST(0.5, art_affinity * decay_factor),
                music_affinity = GREATEST(0.5, music_affinity * decay_factor),
                vid_affinity = GREATEST(0.5, vid_affinity * decay_factor),
                last_decay_at = NOW()
              WHERE last_decay_at < one_day_ago;
            END;
            $body$ LANGUAGE plpgsql;
        $fn$;
    END IF;
END $$;
