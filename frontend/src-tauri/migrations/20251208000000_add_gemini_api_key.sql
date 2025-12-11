-- Add geminiApiKey column to settings table for Google Gemini API support
ALTER TABLE settings ADD COLUMN geminiApiKey TEXT;
