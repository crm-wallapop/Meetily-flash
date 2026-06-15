-- Single embedding model (nemo_titanet). Migrate any legacy value.
UPDATE settings SET speaker_embedding_model = 'nemo_titanet'
WHERE speaker_embedding_model IS NULL OR speaker_embedding_model != 'nemo_titanet';
