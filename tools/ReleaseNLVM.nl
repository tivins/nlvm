namespace tools;

class ReleaseNLVM
{
    private static int run() throws IOException
    {
        try {
            auto release = new Release();

            auto changelogVersion = release.findChangelogVersion();
            
            auto specsVersion = system.io.File.readAllText("SPECS_VERSION").trim();
            auto version = changelogVersion + "+" + specsVersion;
            auto tagMessage = "Release " + changelogVersion + "\n" + "Specs " + specsVersion;
            system.Out.println("Latest version: " + version);

            release.createTag(version, tagMessage);
            system.Out.println("Created tag " + version + ".");
            release.pushTag(version);
            system.Out.println("Pushed tag " + version + " to origin.");
        }
        catch (Exception e) {
            system.Err.println("Error:\n" + e.message);
            return 1;
        }
        return 0;
    }

    public static int main(string[] args)
    {
        try {
            return ReleaseNLVM.run();
        } catch (IOException ex) {
            system.Err.println("Error: " + ex.message);
            return 1;
        }
    }
}
