namespace tools;

class Release 
{
    public construct() {
    }
   
    public string findChangelogVersion() throws IOException 
    {
        string changelog = system.io.File.readAllText("CHANGELOG.md");
        auto header = system.text.Regex.matchFirst("## \\[([^\\]]+)\\]", changelog);
        if (header == null) {
            return "";
        }
        return header.groups[1];
    }

    public void createTag(string version, string tagMessage) throws Exception 
    {
        auto result = system.ps.Process.run(new string[]{"git", "tag", "-a", version, "-m", tagMessage});
        if (result.exitCode != 0) {
            throw new Exception(result.stderr);
        }
    }

    public void pushTag(string version) throws Exception
    {
        auto result = system.ps.Process.run(new string[]{"git", "push", "origin", version});
        if (result.exitCode != 0) {
            throw new Exception(result.stderr);
        }
    }
}
